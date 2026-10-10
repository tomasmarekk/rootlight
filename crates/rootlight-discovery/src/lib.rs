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

mod apex;
mod common_lisp;
mod encoding;
mod fsharp;
mod fsproj;
mod incremental;
mod matlab;
mod objective_c;
mod office;
mod prolog;
mod shebang;
mod vba;

pub use fsproj::{OrderedFSharpCompileMembers, ordered_fsharp_compile_members};

pub use incremental::{
    IncrementalDiscovery, IncrementalDiscoveryBaseline, IncrementalDiscoveryContext,
    IncrementalDiscoveryOptions, IncrementalDiscoveryProgress, ManifestReconcileInputs,
    correlate_incremental_manifest, discover_incremental, discover_incremental_with_progress,
};

/// Current deterministic manifest and source-classification revision.
pub const DISCOVERY_MANIFEST_VERSION: &str = "1.42";
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

/// Whether a source snapshot may undergo native F# admission without a content hint.
///
/// This checks sampled lexical contexts and conflicting Forth evidence only;
/// `true` is not a language signal or a successful parse. The native caller must
/// establish complete sampled syntax and an owned declaration witness before
/// admitting F#. The shared encoding classifier validates UTF-16 and supplies
/// the same prefix bounded by both original and projected
/// [`MAX_CLASSIFICATION_BYTES`]. Incomplete sampled
/// lexical contexts and ambiguous unowned colons return `false`.
///
/// # Errors
///
/// Returns cancellation or source-decoding/resource errors from the shared
/// classifier. Unsupported encodings, invalid sampled UTF-8, incomplete masked
/// contexts, and Forth conflicts return `Ok(false)`; syntax validity is left to
/// the native caller.
pub fn fsharp_native_probe_allowed(
    content: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, DiscoveryError> {
    let allowed = match encoding::classify(content, cancellation)? {
        Some(classification) => {
            let projected = classification
                .decoded_prefix
                .as_ref()
                .map_or(content, |prefix| prefix.as_bytes());
            let sample = projected
                .get(..MAX_CLASSIFICATION_BYTES)
                .unwrap_or(projected);
            fsharp::native_probe_allowed(
                sample,
                classification.prefix_truncated,
                classification.prefix_truncated
                    && (classification.decoded_prefix.is_some()
                        || projected.len() >= MAX_CLASSIFICATION_BYTES),
            )
        }
        None => false,
    };
    cancellation.check()?;
    Ok(allowed)
}

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
    metadata_default: Gitignore,
    host_default: Gitignore,
    vcs_ignore: Gitignore,
    repository: Gitignore,
    operation: Gitignore,
}

impl DiscoveryPolicy {
    /// Builds a layered matcher in deterministic precedence order.
    /// Git metadata remains excluded regardless of later pattern negations.
    ///
    /// # Errors
    ///
    /// Returns [`DiscoveryError::InvalidPolicy`] for unordered layers or unsafe
    /// source labels, and [`DiscoveryError::InvalidPattern`] for malformed
    /// configured patterns.
    pub fn build(mut rules: Vec<PolicyRule>, audit: bool) -> Result<Self, DiscoveryError> {
        let built_in_rules = default_rules();
        let built_in_rule_count = built_in_rules.len();
        rules.splice(0..0, built_in_rules);
        if rules.windows(2).any(|pair| pair[0].layer > pair[1].layer) {
            return Err(DiscoveryError::InvalidPolicy);
        }
        for rule in &rules {
            if !valid_source_label(&rule.source) {
                return Err(DiscoveryError::InvalidPolicy);
            }
        }
        let host_rules = rules
            .get(built_in_rule_count..)
            .ok_or(DiscoveryError::InvalidPolicy)?;
        let matchers = PolicyMatchers {
            default: build_policy_matcher(&rules, PolicyLayer::Default)?,
            metadata_default: build_metadata_matcher()?,
            host_default: build_policy_matcher(host_rules, PolicyLayer::Default)?,
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

    /// Retains tracked regular files through VCS ignores and built-in artifact
    /// directory exclusions, without overriding Git metadata, caller-supplied
    /// default rules, repository, operation, or VFS safety policy.
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

    /// Retains tracked files using their capability-attested existing spelling.
    ///
    /// Windows resolves each regular tracked path through `root` without reading
    /// source content. Missing or unresolvable entries retain only their original
    /// exact policy key; they never confer case-folded or inode-wide admission.
    /// Non-Windows admission remains exact. Git metadata, caller-supplied default
    /// rules, repository and operation rules, and VFS exclusions remain authoritative.
    ///
    /// # Errors
    ///
    /// Returns [`DiscoveryError`] for excessive inventory size, invalid input
    /// paths, cancellation, or unavailable bounded VFS memory. No partially
    /// validated policy is returned.
    pub fn with_tracked_files_in_root(
        mut self,
        root: &RepositoryRoot,
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
            #[cfg(windows)]
            let path = {
                let resolved = root.resolve_regular_path(&path, cancellation);
                cancellation.check()?;
                match resolved {
                    Ok(path) => path,
                    Err(error @ (VfsError::MemoryUnavailable | VfsError::Cancelled(_))) => {
                        return Err(error.into());
                    }
                    Err(_) => path,
                }
            };
            #[cfg(not(windows))]
            let _ = root;
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
        // A later artifact pattern or caller negation cannot reopen Git metadata.
        if let Some(decision) = default_policy_decision(
            &self.matchers.metadata_default,
            path,
            is_directory,
            self.audit,
        )
        .filter(|decision| decision.excluded)
        {
            return decision;
        }
        let default_decision =
            default_policy_decision(&self.matchers.default, path, is_directory, self.audit);
        let default_excluded = default_decision
            .as_ref()
            .is_some_and(|decision| decision.excluded);
        let mut decision = default_decision.unwrap_or_default();

        // Repository-controlled ignores cannot reopen an excluded artifact directory.
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
        } else if !default_policy_decision(&self.matchers.host_default, path, is_directory, false)
            .is_some_and(|decision| decision.excluded)
            && self.admits_tracked_path(path, is_directory)
        {
            // A directory name is not evidence that its tracked leaves are build outputs.
            // Only built-in artifact defaults are soft; caller rules retain their authority.
            decision = PolicyDecision {
                included: true,
                ..PolicyDecision::default()
            };
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

fn build_metadata_matcher() -> Result<Gitignore, DiscoveryError> {
    let rule = metadata_default_rule();
    let mut builder = GitignoreBuilder::new("");
    // Windows Git metadata lookups accept case aliases; ordinary policy remains exact.
    #[cfg(windows)]
    builder
        .case_insensitive(true)
        .map_err(|source| DiscoveryError::InvalidPattern { source })?;
    builder
        .add_line(Some(PathBuf::from(rule.source)), &rule.pattern)
        .map_err(|source| DiscoveryError::InvalidPattern { source })?;
    builder
        .build()
        .map_err(|source| DiscoveryError::InvalidPattern { source })
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

fn default_policy_decision(
    matcher: &Gitignore,
    path: &RelativePath,
    is_directory: bool,
    audit: bool,
) -> Option<PolicyDecision> {
    let mut candidate = path.as_str();
    let mut is_directory = is_directory;
    // Required routes may reopen a default-excluded directory, not its untracked
    // descendants. Each parent is a shorter slice of an already bounded relative path.
    while !candidate.is_empty() {
        if let Some(decision) =
            decision_from_match(matcher.matched(Path::new(candidate), is_directory), audit)
        {
            return Some(decision);
        }
        candidate = parent_scope(candidate);
        is_directory = true;
    }
    None
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
    /// Unanimous source-language evidence from peers sharing an ambiguous suffix.
    Context,
    /// Exact conventional source filename, independent of its directory.
    Filename,
    /// Explicit source-language assignment in the immutable configuration.
    Configuration,
    /// Validated literal declaration from a retained local project document.
    Project,
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

/// Source-bound literal language declaration, not evaluated build membership.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectLanguageWitness {
    /// Canonical language label declared by the project format.
    pub language: String,
    /// Identity of the admitted project document.
    pub project_file: FileId,
    /// Exact admitted repository-relative project path.
    pub project_path: String,
    /// Hash of the complete original project bytes that supplied the declaration.
    pub project_content_hash: ContentHash,
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
    /// BOM-verified UTF-16 byte order, or the legacy UTF-8 interpretation.
    #[serde(default = "legacy_source_encoding")]
    pub encoding: String,
    /// Input classification.
    pub class: InputClass,
    /// Ordered language signals; no signal implies unknown language.
    pub language_signals: Vec<LanguageSignal>,
    /// Complete-document project witnesses joined only to already admitted inputs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub project_language_witnesses: Vec<ProjectLanguageWitness>,
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
    Ok(state.finish()?.manifest)
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
    let (result, scanned) = state.finish_with_scan()?;
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
    state.finish()
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
    project_witnesses: Vec<PendingProjectWitness>,
    project_member_count: usize,
    project_member_bytes: usize,
}

struct PendingProjectWitness {
    file: FileId,
    path: String,
    content_hash: ContentHash,
    members: Vec<String>,
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
            project_witnesses: Vec::new(),
            project_member_count: 0,
            project_member_bytes: 0,
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
        let container_format = office::container_format(path.as_str(), snapshot.content());
        let classification = if container_format.is_some() {
            Some(encoding::SourceClassification {
                encoding: "binary",
                decoded_prefix: None,
                prefix_truncated: false,
            })
        } else {
            encoding::classify(snapshot.content(), self.cancellation)?
        };
        let Some(classification) = classification else {
            self.exclude(&path, ExclusionReason::Binary, decisive_rule);
            return Ok(());
        };
        let classification_content = classification
            .decoded_prefix
            .as_ref()
            .map_or(snapshot.content(), |prefix| prefix.as_bytes());
        let (class, mut language_signals) = classify_prefix(
            &path,
            if container_format.is_some() {
                b""
            } else {
                classification_content
            },
            classification.prefix_truncated,
            classification.decoded_prefix.is_some(),
        );
        if let Some(format) = container_format {
            language_signals.clear();
            language_signals.push(LanguageSignal {
                language: format.to_owned(),
                evidence: LanguageEvidence::Content,
            });
        }
        if let Some(language) = self.config.source_language_for_path(path.as_str()) {
            language_signals.push(LanguageSignal {
                language: language.to_owned(),
                evidence: LanguageEvidence::Configuration,
            });
            language_signals.sort();
        }
        let file = snapshot.file();
        self.record_project_witness(&path, &snapshot)?;
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
            encoding: classification.encoding.to_owned(),
            class,
            language_signals,
            project_language_witnesses: Vec::new(),
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

    fn record_project_witness(
        &mut self,
        path: &RelativePath,
        snapshot: &SourceSnapshot,
    ) -> Result<(), DiscoveryError> {
        if !Path::new(path.as_str())
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|suffix| suffix.eq_ignore_ascii_case("fsproj"))
        {
            return Ok(());
        }
        let members =
            match fsproj::compile_members(snapshot.content(), path.as_str(), self.cancellation)? {
                fsproj::CompileMembers::Valid(members) => members,
                fsproj::CompileMembers::Rejected => {
                    self.diagnostic(Some(path), "DISCOVERY_PROJECT_WITNESS_UNPROVEN");
                    return Ok(());
                }
                fsproj::CompileMembers::Bounded => {
                    self.diagnostic(Some(path), "DISCOVERY_PROJECT_WITNESS_LIMIT");
                    return Ok(());
                }
            };
        if members.is_empty() {
            return Ok(());
        }
        // Charge both pending member paths and their eventual repeated project
        // provenance before retaining anything from this document. A bound must
        // not expose a misleading prefix of its declarations.
        let count = self.project_member_count.checked_add(members.len());
        let bytes = self
            .project_member_bytes
            .checked_add(path.as_str().len())
            .and_then(|initial| {
                members.iter().try_fold(initial, |total, member| {
                    total
                        .checked_add(member.len())?
                        .checked_add(path.as_str().len())?
                        .checked_add("fsharp".len() * 2)
                })
            });
        let (Some(count), Some(bytes)) = (count, bytes) else {
            self.diagnostic(Some(path), "DISCOVERY_PROJECT_WITNESS_LIMIT");
            return Ok(());
        };
        if count > fsproj::MAX_PROJECT_MEMBERS
            || bytes > fsproj::MAX_PROJECT_MEMBER_BYTES
            || path.as_str().len() > fsproj::MAX_PROJECT_STRING_BYTES
        {
            self.diagnostic(Some(path), "DISCOVERY_PROJECT_WITNESS_LIMIT");
            return Ok(());
        }
        self.project_witnesses
            .try_reserve(1)
            .map_err(|_| DiscoveryError::Vfs(VfsError::MemoryUnavailable))?;
        self.project_witnesses.push(PendingProjectWitness {
            file: snapshot.file(),
            path: copy_project_string(path.as_str())?,
            content_hash: snapshot.content_hash(),
            members,
        });
        self.project_member_count = count;
        self.project_member_bytes = bytes;
        Ok(())
    }

    fn apply_project_witnesses(&mut self) -> Result<(), DiscoveryError> {
        if self.project_witnesses.is_empty() {
            return Ok(());
        }
        // Match the public witness's canonical ordering before fan-out, rather
        // than rescanning every admitted source just to sort sparse metadata.
        let mut projects = std::mem::take(&mut self.project_witnesses);
        projects.sort_unstable_by(|left, right| {
            left.file
                .cmp(&right.file)
                .then_with(|| left.path.cmp(&right.path))
                .then_with(|| left.content_hash.cmp(&right.content_hash))
        });
        for project in projects {
            self.cancellation.check()?;
            let mut unmatched = false;
            for member in project.members {
                self.cancellation.check()?;
                // A declaration never opens a path or expands policy admission.
                // Exact retained spelling is required; case-folding would be a
                // new filesystem identity claim without VFS attestation.
                let Ok(index) = self
                    .inputs
                    .binary_search_by(|input| input.path.cmp(&member))
                else {
                    unmatched = true;
                    continue;
                };
                let input = &mut self.inputs[index];
                input
                    .project_language_witnesses
                    .try_reserve(1)
                    .map_err(|_| DiscoveryError::Vfs(VfsError::MemoryUnavailable))?;
                input
                    .project_language_witnesses
                    .push(ProjectLanguageWitness {
                        language: copy_project_string("fsharp")?,
                        project_file: project.file,
                        project_path: copy_project_string(&project.path)?,
                        project_content_hash: project.content_hash,
                    });
                let signal = LanguageSignal {
                    language: copy_project_string("fsharp")?,
                    evidence: LanguageEvidence::Project,
                };
                if !input.language_signals.contains(&signal) {
                    input
                        .language_signals
                        .try_reserve(1)
                        .map_err(|_| DiscoveryError::Vfs(VfsError::MemoryUnavailable))?;
                    input.language_signals.push(signal);
                    input.language_signals.sort();
                }
            }
            if unmatched {
                let path = RelativePath::parse(Path::new(&project.path))?;
                self.diagnostic(Some(&path), "DISCOVERY_PROJECT_MEMBER_NOT_ADMITTED");
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<DiscoveryResult, DiscoveryError> {
        self.finish_with_scan().map(|(result, _)| result)
    }

    fn finish_with_scan(mut self) -> Result<(DiscoveryResult, Vec<ScannedFile>), DiscoveryError> {
        self.cancellation.check()?;
        self.inputs.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then_with(|| left.file.cmp(&right.file))
        });
        self.apply_project_witnesses()?;
        infer_shared_m_suffix_context(&mut self.inputs);
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
        Ok((
            DiscoveryResult {
                manifest,
                snapshots: self.snapshots,
            },
            self.scanned,
        ))
    }
}

fn copy_project_string(value: &str) -> Result<String, DiscoveryError> {
    let mut copy = String::new();
    copy.try_reserve_exact(value.len())
        .map_err(|_| DiscoveryError::Vfs(VfsError::MemoryUnavailable))?;
    copy.push_str(value);
    Ok(copy)
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

#[cfg(test)]
fn classify(path: &RelativePath, content: &[u8]) -> (InputClass, Vec<LanguageSignal>) {
    classify_prefix(
        path,
        content,
        content.len() > MAX_CLASSIFICATION_BYTES,
        false,
    )
}

fn classify_prefix(
    path: &RelativePath,
    content: &[u8],
    prefix_truncated: bool,
    decoded_prefix: bool,
) -> (InputClass, Vec<LanguageSignal>) {
    let normalized = path.as_str().to_ascii_lowercase();
    let class = if generated_path(&normalized) || generated_content(content) {
        InputClass::Generated
    } else if vendored_path(&normalized) {
        InputClass::Vendored
    } else {
        InputClass::Source
    };

    let mut signals = BTreeSet::new();
    let extension = extension_language(&normalized);
    if let Some(language) = extension {
        signals.insert(LanguageSignal {
            language: language.to_owned(),
            evidence: LanguageEvidence::Extension,
        });
    }
    if let Some(language) = filename_language(&normalized) {
        signals.insert(LanguageSignal {
            language: language.to_owned(),
            evidence: LanguageEvidence::Filename,
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
    if common_lisp::is_content_candidate(&normalized)
        && common_lisp::has_content_hint(
            &content[..content.len().min(MAX_CLASSIFICATION_BYTES)],
            prefix_truncated,
        )
    {
        signals.insert(LanguageSignal {
            language: "common-lisp".to_owned(),
            evidence: LanguageEvidence::Content,
        });
    }
    let content_hint = if Path::new(&normalized)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("fs"))
    {
        fsharp::has_content_hint(
            &content[..content.len().min(MAX_CLASSIFICATION_BYTES)],
            prefix_truncated,
            prefix_truncated && (decoded_prefix || content.len() >= MAX_CLASSIFICATION_BYTES),
        )
        .then_some("fsharp")
    } else if is_ambiguous_perl_prolog_source(&normalized)
        && prolog::has_content_hint(&content[..content.len().min(MAX_CLASSIFICATION_BYTES)])
    {
        Some("prolog")
    } else if is_ambiguous_basic_source(&normalized) {
        let sample = &content[..content.len().min(MAX_CLASSIFICATION_BYTES)];
        // A class export shares its suffix with Apex. Retain both independent
        // hints when they conflict; parser availability must not choose one.
        if normalized.ends_with(".cls") && apex::has_content_hint(sample, prefix_truncated) {
            signals.insert(LanguageSignal {
                language: "apex".to_owned(),
                evidence: LanguageEvidence::Content,
            });
        }
        if vba::has_content_hint(sample) {
            Some("vba")
        } else {
            vba::has_classic_content_hint(sample).then_some("visual-basic-classic")
        }
    } else if extension == Some("objective-c") {
        content_language_with_scripts(content, true)
    } else {
        content_language(content)
    };
    if let Some(language) = content_hint {
        signals.insert(LanguageSignal {
            language: language.to_owned(),
            evidence: LanguageEvidence::Content,
        });
    }
    (class, signals.into_iter().collect())
}

fn infer_shared_m_suffix_context(inputs: &mut [ManifestInput]) {
    let mut proven = None;
    let mut proven_count = 0;
    for input in inputs.iter().filter(|input| is_shared_m_source(input)) {
        if input
            .language_signals
            .iter()
            .any(|signal| signal.evidence == LanguageEvidence::Configuration)
        {
            continue;
        }
        let mut explicit = None;
        for signal in input
            .language_signals
            .iter()
            .filter(|signal| signal.evidence == LanguageEvidence::Content)
        {
            let language = match signal.language.as_str() {
                "matlab" => "matlab",
                "objective-c" => "objective-c",
                _ => return,
            };
            if explicit.is_some_and(|previous| previous != language) {
                return;
            }
            explicit = Some(language);
        }
        if let Some(language) = explicit {
            if proven.is_some_and(|previous| previous != language) {
                return;
            }
            proven = Some(language);
            proven_count += 1;
        }
    }
    // One source cannot establish the language of an otherwise ambiguous
    // repository. Mixed explicit evidence must leave each undecidable file's
    // original extension fallback intact.
    if proven_count < 2 {
        return;
    }
    let language = proven.expect("two explicit peers establish one language");
    for input in inputs.iter_mut().filter(|input| is_shared_m_source(input)) {
        if input.language_signals.iter().any(|signal| {
            matches!(
                signal.evidence,
                LanguageEvidence::Content | LanguageEvidence::Configuration
            )
        }) {
            continue;
        }
        input.language_signals.push(LanguageSignal {
            language: language.to_owned(),
            evidence: LanguageEvidence::Context,
        });
        input.language_signals.sort();
    }
}

fn is_shared_m_source(input: &ManifestInput) -> bool {
    matches!(input.encoding.as_str(), "utf-8" | "utf-16le" | "utf-16be")
        && input
            .path
            .rsplit_once('.')
            .is_some_and(|(_, suffix)| suffix.eq_ignore_ascii_case("m"))
}

fn legacy_source_encoding() -> String {
    "utf-8".to_owned()
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
        language: "agda",
        suffixes: &[".agda", ".lagda", ".lagda.md", ".lagda.rst", ".lagda.tex"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["agda-native-structural-1"],
    },
    LanguageCapability {
        language: "apex",
        suffixes: &[],
        aliases: &[],
        detectors: &["content", "configuration"],
        maximum_tier: "tier_d",
        analyzers: &["treesitter"],
    },
    LanguageCapability {
        language: "assembly",
        suffixes: &[".asm", ".s"],
        aliases: &["asm"],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["treesitter"],
    },
    LanguageCapability {
        language: "astro",
        suffixes: &[".astro"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["treesitter"],
    },
    LanguageCapability {
        language: "awk",
        suffixes: &[".awk"],
        aliases: &["gawk", "mawk", "nawk"],
        detectors: &["extension", "shebang"],
        maximum_tier: "tier_d",
        analyzers: &["native"],
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
        language: "carbon",
        suffixes: &[".carbon"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["source-fallback"],
    },
    LanguageCapability {
        language: "cobol",
        suffixes: &[".cbl", ".cob"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["source-fallback"],
    },
    LanguageCapability {
        language: "cobol-fixed",
        suffixes: &[],
        aliases: &[],
        detectors: &["configuration"],
        maximum_tier: "tier_d",
        analyzers: &["treesitter"],
    },
    LanguageCapability {
        language: "cobol-free",
        suffixes: &[],
        aliases: &[],
        detectors: &["configuration"],
        maximum_tier: "tier_d",
        analyzers: &["treesitter"],
    },
    LanguageCapability {
        language: "common-lisp",
        suffixes: &[".lisp", ".asd"],
        aliases: &["commonlisp", "common_lisp"],
        detectors: &["content", "extension"],
        maximum_tier: "tier_d",
        analyzers: &["treesitter"],
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
        maximum_tier: "tier_b",
        analyzers: &["treesitter", "project-adapter"],
    },
    LanguageCapability {
        language: "elm",
        suffixes: &[".elm"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["native"],
    },
    LanguageCapability {
        language: "erlang",
        suffixes: &[".app.src", ".erl", ".hrl"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["native"],
    },
    LanguageCapability {
        language: "fortran",
        suffixes: &[".f90", ".f95", ".f03", ".f08", ".fypp"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["treesitter"],
    },
    LanguageCapability {
        language: "fsharp",
        // `.fs` also denotes Forth; a bounded content hint or assignment is required.
        suffixes: &[".fsscript", ".fsx"],
        aliases: &[],
        detectors: &["content", "extension", "configuration"],
        maximum_tier: "tier_d",
        analyzers: &["treesitter"],
    },
    LanguageCapability {
        language: "fsharp-signature",
        suffixes: &[".fsi"],
        aliases: &[],
        detectors: &["extension", "configuration"],
        maximum_tier: "tier_d",
        analyzers: &["treesitter"],
    },
    LanguageCapability {
        language: "glsl",
        suffixes: &[".vert", ".frag", ".vsh", ".fsh", ".glsl"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["treesitter"],
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
        suffixes: &[".gradle", ".groovy", ".gsh", ".gvy", ".gy", ".jenkins"],
        aliases: &[],
        detectors: &["extension", "filename", "shebang"],
        maximum_tier: "tier_d",
        analyzers: &["treesitter"],
    },
    LanguageCapability {
        language: "haskell",
        suffixes: &[".hs"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["treesitter"],
    },
    LanguageCapability {
        language: "hlsl",
        suffixes: &[".hlsl", ".hlsli", ".fxh"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["treesitter"],
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
        language: "idris",
        suffixes: &[".idr", ".lidr"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["source-fallback"],
    },
    LanguageCapability {
        language: "ini",
        suffixes: &[".ini"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["native"],
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
        language: "lean",
        suffixes: &[".lean"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["source-fallback"],
    },
    LanguageCapability {
        language: "less",
        suffixes: &[".less"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["source-fallback"],
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
        language: "make",
        suffixes: &[".mak", ".mk"],
        aliases: &["makefile"],
        detectors: &["filename", "extension"],
        maximum_tier: "tier_d",
        analyzers: &["native"],
    },
    LanguageCapability {
        language: "markdown",
        suffixes: &[".markdown", ".md"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["treesitter"],
    },
    LanguageCapability {
        language: "matlab",
        suffixes: &[".mlx"],
        aliases: &[],
        detectors: &["content", "context", "extension"],
        maximum_tier: "tier_d",
        analyzers: &["treesitter"],
    },
    LanguageCapability {
        language: "objective-c",
        suffixes: &[".m"],
        aliases: &["objc"],
        detectors: &["content", "context", "extension"],
        maximum_tier: "tier_d",
        analyzers: &["treesitter"],
    },
    LanguageCapability {
        language: "nix",
        suffixes: &[".nix"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["treesitter"],
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
        language: "pascal",
        suffixes: &[".pas", ".dpr", ".lpr"],
        aliases: &["delphi", "object-pascal"],
        detectors: &["extension", "configuration"],
        maximum_tier: "tier_d",
        analyzers: &["source-fallback"],
    },
    LanguageCapability {
        language: "perl",
        suffixes: &[".pl", ".pm", ".pod"],
        aliases: &[],
        detectors: &["extension", "shebang"],
        maximum_tier: "tier_d",
        analyzers: &["treesitter"],
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
        language: "plsql",
        suffixes: &[".pks", ".pkb", ".plb", ".pls"],
        aliases: &["pl-sql"],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["treesitter"],
    },
    LanguageCapability {
        language: "powershell",
        suffixes: &[".ps1", ".psd1", ".psm1"],
        aliases: &["pwsh"],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["treesitter"],
    },
    LanguageCapability {
        language: "pony",
        suffixes: &[".pony"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["source-fallback"],
    },
    LanguageCapability {
        language: "prolog",
        suffixes: &[],
        aliases: &[],
        detectors: &["content"],
        maximum_tier: "tier_d",
        analyzers: &["native"],
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
        analyzers: &["treesitter"],
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
        language: "sas",
        suffixes: &[".sas"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["source-fallback"],
    },
    LanguageCapability {
        language: "sass",
        suffixes: &[".sass"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["source-fallback"],
    },
    LanguageCapability {
        language: "scala",
        suffixes: &[".sc", ".scala", ".sbt"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["treesitter"],
    },
    LanguageCapability {
        language: "scss",
        suffixes: &[".scss"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["native"],
    },
    LanguageCapability {
        language: "solidity",
        suffixes: &[".sol"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["treesitter"],
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
        language: "tcl",
        suffixes: &[".tcl", ".tk"],
        aliases: &["tk"],
        detectors: &["extension", "shebang"],
        maximum_tier: "tier_d",
        analyzers: &["native"],
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
        language: "vba",
        suffixes: &[],
        aliases: &[],
        detectors: &["content"],
        maximum_tier: "tier_d",
        analyzers: &["treesitter"],
    },
    LanguageCapability {
        language: "visual-basic-classic",
        suffixes: &[],
        aliases: &[],
        detectors: &["content"],
        maximum_tier: "tier_d",
        analyzers: &["treesitter"],
    },
    LanguageCapability {
        language: "wgsl",
        suffixes: &[".wgsl"],
        aliases: &[],
        detectors: &["extension"],
        maximum_tier: "tier_d",
        analyzers: &["treesitter"],
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

/// Returns the source language of an exact conventional basename.
///
/// Matching is ASCII-case-insensitive and accepts either path separator. Backup
/// suffixes and directory names do not match; no file contents are inspected.
#[must_use]
pub fn filename_language(path: &str) -> Option<&'static str> {
    let name = path.rsplit(['/', '\\']).next()?;
    if name.eq_ignore_ascii_case("Jenkinsfile") {
        Some("groovy")
    } else if ["Makefile", "GNUmakefile", "BSDmakefile"]
        .iter()
        .any(|basename| name.eq_ignore_ascii_case(basename))
    {
        Some("make")
    } else {
        None
    }
}

/// Reports a BASIC-family suffix that needs dialect evidence before parser admission.
///
/// These case-insensitive suffixes are shared by VBA, other BASIC dialects and
/// other source formats. They never establish a language by themselves.
#[must_use]
pub fn is_ambiguous_basic_source(path: &str) -> bool {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    name.rsplit_once('.').is_some_and(|(_, extension)| {
        ["bas", "cls", "frm"]
            .iter()
            .any(|suffix| extension.eq_ignore_ascii_case(suffix))
    })
}

/// Reports the shared Perl/Prolog source suffix without claiming either syntax.
#[must_use]
pub fn is_ambiguous_perl_prolog_source(path: &str) -> bool {
    path.rsplit(['/', '\\'])
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("pl"))
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
    shebang::language(content)
}

fn content_language(content: &[u8]) -> Option<&'static str> {
    content_language_with_scripts(content, false)
}

fn content_language_with_scripts(content: &[u8], matlab_scripts: bool) -> Option<&'static str> {
    let sample = content.get(..content.len().min(MAX_CLASSIFICATION_BYTES))?;
    let text = String::from_utf8_lossy(sample);
    let matlab = matlab::hints(sample);
    if matlab.declaration || matlab_scripts && matlab.script {
        Some("matlab")
    } else if objective_c::has_content_hint(sample) {
        Some("objective-c")
    } else if text.contains("fn main(") || text.contains("pub struct ") {
        Some("rust")
    } else if text.contains("package main") && text.contains("func ") {
        Some("go")
    } else if text.lines().any(has_python_function_hint) {
        Some("python")
    } else if looks_like_cpp(&text) {
        Some("cpp")
    } else {
        None
    }
}

fn has_python_function_hint(line: &str) -> bool {
    let statement = line.trim_start();
    let Some(declaration) = statement
        .strip_prefix("def ")
        .or_else(|| statement.strip_prefix("async def "))
    else {
        return false;
    };
    let declaration = declaration.trim_start();
    if !declaration
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_' || !byte.is_ascii())
    {
        return false;
    }
    // This is a routing hint, not Python identifier validation. Keep Unicode
    // names intact while rejecting assignments and member expressions before
    // the parameter list; a colon inside a Groovy initializer is not evidence.
    let name_bytes = declaration
        .bytes()
        .take_while(|byte| byte.is_ascii_alphanumeric() || *byte == b'_' || !byte.is_ascii())
        .count();
    let Some(signature) = declaration.get(name_bytes..).map(str::trim_start) else {
        return false;
    };
    // Type parameters may precede the value parameters in Python signatures.
    signature.starts_with(['(', '[']) && signature.contains('(') && signature.contains(':')
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

fn metadata_default_rule() -> PolicyRule {
    PolicyRule {
        layer: PolicyLayer::Default,
        pattern: ".git".to_owned(),
        source: "rootlight-default".to_owned(),
    }
}

fn default_rules() -> Vec<PolicyRule> {
    std::iter::once(metadata_default_rule())
        .chain(
            ["target/", "node_modules/", ".venv/", "dist/", "build/"]
                .into_iter()
                .map(|pattern| PolicyRule {
                    layer: PolicyLayer::Default,
                    pattern: pattern.to_owned(),
                    source: "rootlight-default".to_owned(),
                }),
        )
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
    fn project_witnesses_bind_complete_bytes_in_all_discovery_modes() {
        let temporary = local_tempdir();
        let project = b"<Project Sdk='Microsoft.NET.Sdk'><ItemGroup><Compile Include='../src/Unit.fs'/></ItemGroup></Project>";
        write_fixture(&temporary, "project/App.fsproj", project);
        write_fixture(&temporary, "src/Unit.fs", b"let answer = 1\n");
        write_fixture(&temporary, "src/Other.fs", b"let other = 2\n");
        let root = fixture_root(&temporary, b"literal-project-witness");
        let policy = DiscoveryPolicy::build(Vec::new(), false).unwrap();
        let config = config();
        let cancellation = Cancellation::new();
        let manifest = discover(&root, &config, &policy, limits(), &cancellation).unwrap();
        let (retained, snapshots) = discover_with_snapshots(
            &root,
            &config,
            &policy,
            limits(),
            BTreeMap::new(),
            &cancellation,
        )
        .unwrap()
        .into_parts();
        assert_eq!(manifest, retained);
        assert_eq!(snapshots.len(), manifest.inputs.len());
        let context = IncrementalDiscoveryContext::new(
            config.hash(),
            rootlight_ids::derive_fact("test.project-provider", b"provider").id(),
            content_hash(b"provider-revision"),
        );
        let (reconciled, _) = discover_manifest_and_reconcile_with_progress(
            &root,
            &config,
            ManifestReconcileInputs::new(None, context),
            &policy,
            limits(),
            &cancellation,
            |_| {},
        )
        .unwrap();
        assert_eq!(manifest, reconciled);
        let project_input = manifest
            .inputs
            .iter()
            .find(|input| input.path == "project/App.fsproj")
            .unwrap();
        let member = manifest
            .inputs
            .iter()
            .find(|input| input.path == "src/Unit.fs")
            .unwrap();
        assert_eq!(
            member.project_language_witnesses,
            [ProjectLanguageWitness {
                language: "fsharp".to_owned(),
                project_file: project_input.file,
                project_path: project_input.path.clone(),
                project_content_hash: content_hash(project),
            }]
        );
        assert_eq!(
            member.language_signals,
            [LanguageSignal {
                language: "fsharp".to_owned(),
                evidence: LanguageEvidence::Project,
            }]
        );
        assert!(
            manifest
                .inputs
                .iter()
                .find(|input| input.path == "src/Other.fs")
                .unwrap()
                .project_language_witnesses
                .is_empty()
        );
        assert!(project_input.project_language_witnesses.is_empty());

        let audit_policy = DiscoveryPolicy::build(Vec::new(), true).unwrap();
        let audited = discover(&root, &config, &audit_policy, limits(), &cancellation).unwrap();
        assert_eq!(
            audited
                .inputs
                .iter()
                .map(|input| &input.project_language_witnesses)
                .collect::<Vec<_>>(),
            manifest
                .inputs
                .iter()
                .map(|input| &input.project_language_witnesses)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn conditioned_project_declarations_only_hint_admitted_literal_members() {
        let temporary = local_tempdir();
        write_fixture(&temporary, "Unit.fs", b"let answer = 1\n");
        write_fixture(&temporary, "Other.fs", b"let other = 2\n");
        write_fixture(&temporary, "cache/Hidden.fs", b"let hidden = 3\n");
        write_fixture(&temporary, ".gitignore", b"cache/\n");
        let root = fixture_root(&temporary, b"conditioned-project-language-evidence");
        let policy = DiscoveryPolicy::build(Vec::new(), true).unwrap();
        let config = config();
        for document in [
            "<Project Condition='false'><ItemGroup><Compile Include='Unit.fs'/><Compile Include='cache/Hidden.fs'/><Compile Include='Missing.fs'/></ItemGroup></Project>",
            "<Project><ItemGroup Condition='false'><Compile Include='Unit.fs'/><Compile Include='cache/Hidden.fs'/><Compile Include='Missing.fs'/></ItemGroup></Project>",
            "<Project><ItemGroup><Compile Condition='false' Include='Unit.fs'/><Compile Include='cache/Hidden.fs'/><Compile Include='Missing.fs'/></ItemGroup></Project>",
        ] {
            write_fixture(&temporary, "App.fsproj", document.as_bytes());
            let manifest =
                discover(&root, &config, &policy, limits(), &Cancellation::new()).unwrap();
            let member = manifest
                .inputs
                .iter()
                .find(|input| input.path == "Unit.fs")
                .unwrap();
            assert_eq!(
                member.language_signals,
                [LanguageSignal {
                    language: "fsharp".to_owned(),
                    evidence: LanguageEvidence::Project,
                }]
            );
            let project = manifest
                .inputs
                .iter()
                .find(|input| input.path == "App.fsproj")
                .unwrap();
            assert_eq!(
                member.project_language_witnesses,
                [ProjectLanguageWitness {
                    language: "fsharp".to_owned(),
                    project_file: project.file,
                    project_path: project.path.clone(),
                    project_content_hash: content_hash(document.as_bytes()),
                }]
            );
            assert!(
                manifest
                    .inputs
                    .iter()
                    .find(|input| input.path == "Other.fs")
                    .unwrap()
                    .project_language_witnesses
                    .is_empty()
            );
            assert!(
                !manifest
                    .inputs
                    .iter()
                    .any(|input| matches!(input.path.as_str(), "cache/Hidden.fs" | "Missing.fs"))
            );
            assert!(
                !manifest
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == "DISCOVERY_PROJECT_WITNESS_UNPROVEN")
            );
        }
    }

    #[test]
    fn project_witnesses_never_expand_admission_or_use_invalid_documents() {
        let temporary = local_tempdir();
        write_fixture(&temporary, "Unit.fs", b"let answer = 1\n");
        write_fixture(&temporary, "cache/Hidden.fs", b"let hidden = 2\n");
        write_fixture(&temporary, ".gitignore", b"cache/\n");
        let root = fixture_root(&temporary, b"project-policy-boundary");
        let policy = DiscoveryPolicy::build(Vec::new(), true).unwrap();
        let config = config();
        let cancellation = Cancellation::new();
        for document in [
            "<Project><ItemGroup><Compile Include='Unit.fs'/></ItemGroup></Project><extra/>",
            "<Project><ItemGroup><Compile Include='*.fs'/></ItemGroup></Project>",
            "<Project><ItemGroup><Compile Include='Unit.fs'/><Compile Remove='Unit.fs'/></ItemGroup></Project>",
        ] {
            write_fixture(&temporary, "App.fsproj", document.as_bytes());
            let manifest = discover(&root, &config, &policy, limits(), &cancellation).unwrap();
            assert!(
                manifest
                    .inputs
                    .iter()
                    .all(|input| input.project_language_witnesses.is_empty())
            );
            assert!(
                manifest
                    .inputs
                    .iter()
                    .any(|input| input.path == "App.fsproj")
            );
            assert!(
                manifest
                    .diagnostics
                    .iter()
                    .any(
                        |diagnostic| diagnostic.path.as_deref() == Some("App.fsproj")
                            && diagnostic.code == "DISCOVERY_PROJECT_WITNESS_UNPROVEN"
                    )
            );
        }
        write_fixture(&temporary, "App.fsproj", b"<Project><ItemGroup><Compile Include='Unit.fs'/><Compile Include='cache/Hidden.fs'/><Compile Include='Missing.fs'/></ItemGroup></Project>");
        let manifest = discover(&root, &config, &policy, limits(), &cancellation).unwrap();
        assert!(manifest.inputs.iter().any(|input| input.path == "Unit.fs" && input.project_language_witnesses.len() == 1));
        assert!(
            !manifest
                .inputs
                .iter()
                .any(|input| matches!(input.path.as_str(), "cache/Hidden.fs" | "Missing.fs"))
        );
        assert!(
            manifest
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "DISCOVERY_PROJECT_MEMBER_NOT_ADMITTED")
        );
        assert!(
            manifest
                .exclusions
                .iter()
                .any(|exclusion| exclusion.path == "cache"
                    && exclusion.reason == ExclusionReason::Policy)
        );
    }

    #[test]
    fn project_witness_budget_never_publishes_a_document_prefix() {
        let temporary = local_tempdir();
        write_fixture(&temporary, "Unit.fs", b"let answer = 1\n");
        let mut document = String::from("<Project><ItemGroup><Compile Include='Unit.fs'/>");
        for index in 0..4096 {
            use std::fmt::Write as _;
            write!(&mut document, "<Compile Include='Missing{index}.fs'/>").unwrap();
        }
        document.push_str("</ItemGroup></Project>");
        for path in ["First.fsproj", "Second.fsproj"] {
            write_fixture(&temporary, path, document.as_bytes());
        }
        let root = fixture_root(&temporary, b"aggregate-project-witness-bound");
        let policy = DiscoveryPolicy::build(Vec::new(), false).unwrap();
        let manifest = discover(&root, &config(), &policy, limits(), &Cancellation::new()).unwrap();
        let member = manifest
            .inputs
            .iter()
            .find(|input| input.path == "Unit.fs")
            .unwrap();
        assert_eq!(member.project_language_witnesses.len(), 1);
        assert_eq!(
            member.project_language_witnesses[0].project_path,
            "First.fsproj"
        );
        assert_eq!(manifest.inputs.len(), 3);
        assert!(manifest.coverage.complete);
        assert!(
            manifest
                .diagnostics
                .iter()
                .any(
                    |diagnostic| diagnostic.path.as_deref() == Some("Second.fsproj")
                        && diagnostic.code == "DISCOVERY_PROJECT_WITNESS_LIMIT"
                )
        );
    }

    #[test]
    fn project_witnesses_finalize_successful_truncated_discovery() {
        let temporary = local_tempdir();
        write_fixture(
            &temporary,
            "First.fsproj",
            b"<Project><ItemGroup><Compile Include='Unit.fs'/></ItemGroup></Project>",
        );
        write_fixture(&temporary, "Unit.fs", b"let answer = 1\n");
        write_fixture(&temporary, "Zebra.fs", b"let other = 2\n");
        let root = fixture_root(&temporary, b"truncated-project-witness");
        let policy = DiscoveryPolicy::build(Vec::new(), false).unwrap();
        let entry_limits = DiscoveryLimits::new(2, 16, 1024, 100).unwrap();
        let manifest = discover(
            &root,
            &config(),
            &policy,
            entry_limits,
            &Cancellation::new(),
        )
        .unwrap();
        assert!(!manifest.coverage.complete);
        assert_eq!(manifest.inputs.len(), 2);
        assert_eq!(manifest.inputs[1].project_language_witnesses.len(), 1);
    }

    #[test]
    fn project_witnesses_are_canonical_and_reject_cancelled_finalization() {
        let temporary = local_tempdir();
        write_fixture(&temporary, "Unit.fs", b"let answer = 1\n");
        for path in ["First.fsproj", "Second.fsproj"] {
            write_fixture(&temporary, path, b"<Project><ItemGroup><Compile Include='Unit.fs'/><Compile Include='./Unit.fs'/></ItemGroup></Project>");
        }
        let root = fixture_root(&temporary, b"canonical-project-witnesses");
        let policy = DiscoveryPolicy::build(Vec::new(), false).unwrap();
        let config = config();
        let cancellation = Cancellation::new();
        let manifest = discover(&root, &config, &policy, limits(), &cancellation).unwrap();
        let member = manifest
            .inputs
            .iter()
            .find(|input| input.path == "Unit.fs")
            .unwrap();
        assert_eq!(member.project_language_witnesses.len(), 2);
        assert!(
            member
                .project_language_witnesses
                .windows(2)
                .all(|pair| pair[0] < pair[1])
        );
        let mut legacy = serde_json::to_value(member).unwrap();
        legacy
            .as_object_mut()
            .unwrap()
            .remove("project_language_witnesses");
        assert!(
            serde_json::from_value::<ManifestInput>(legacy)
                .unwrap()
                .project_language_witnesses
                .is_empty()
        );

        let mut state = DiscoveryState::new(
            &root,
            &config,
            &policy,
            limits(),
            SnapshotRetentionOptions::manifest_only(),
            None,
            &cancellation,
        )
        .unwrap();
        state.run().unwrap();
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        assert!(matches!(state.finish(), Err(DiscoveryError::Cancelled(_))));
    }

    #[test]
    fn binary_format_signatures_override_source_suffixes() {
        let temporary = local_tempdir();
        let pdf = b"%PDF-1.7\n1 0 obj\n<< /Type /Catalog >>\nendobj\n";
        for path in ["document.data", "misleading.rs"] {
            write_fixture(&temporary, path, pdf);
        }
        write_fixture(&temporary, "archive.txt", b"PK\x03\x04opaque archive");
        write_fixture(&temporary, "source.pdf", b"let signature = \"%PDF-1.7\";\n");
        let root = fixture_root(&temporary, b"binary-format-signatures");
        let policy = DiscoveryPolicy::build(Vec::new(), true).unwrap();
        let manifest = discover(&root, &config(), &policy, limits(), &Cancellation::new()).unwrap();
        assert_eq!(manifest.inputs.len(), 1);
        assert_eq!(manifest.inputs[0].path, "source.pdf");
        assert_eq!(manifest.inputs[0].encoding, "utf-8");
        for path in ["document.data", "misleading.rs", "archive.txt"] {
            assert!(
                manifest
                    .exclusions
                    .iter()
                    .any(|entry| { entry.path == path && entry.reason == ExclusionReason::Binary })
            );
        }
        assert_eq!(
            manifest,
            discover_manifest_streaming(&root, &config(), &policy, limits(), &Cancellation::new())
                .unwrap()
        );
    }

    #[test]
    fn office_container_candidates_retain_bytes_without_claiming_text() {
        let temporary = local_tempdir();
        let zip = b"PK\x03\x04\0opaque package payload";
        let compound = b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1\0opaque compound payload";
        for (path, bytes) in [
            ("book.XLSM", zip.as_slice()),
            ("legacy.doc", compound.as_slice()),
            ("unrelated.data", zip.as_slice()),
            ("image.xlsm", b"\0not a zip".as_slice()),
        ] {
            write_fixture(&temporary, path, bytes);
        }
        let root = fixture_root(&temporary, b"office-container-admission");
        let policy = DiscoveryPolicy::build(Vec::new(), true).unwrap();
        let (manifest, snapshots) = discover_with_snapshots_at_limit(
            &root,
            &config(),
            &policy,
            limits(),
            BTreeMap::new(),
            4096,
            &Cancellation::new(),
        )
        .unwrap()
        .into_parts();
        assert_eq!(manifest.inputs.len(), 2);
        for (path, bytes, format) in [
            ("book.XLSM", zip.as_slice(), "office-open-xml"),
            ("legacy.doc", compound.as_slice(), "compound-document"),
        ] {
            let input = manifest
                .inputs
                .iter()
                .find(|input| input.path == path)
                .unwrap();
            assert_eq!(input.encoding, "binary");
            assert_eq!(input.bytes, u64::try_from(bytes.len()).unwrap());
            assert_eq!(input.content_hash, content_hash(bytes));
            assert_eq!(snapshots[&input.file].content(), bytes);
            assert_eq!(
                input.language_signals,
                vec![LanguageSignal {
                    language: format.to_owned(),
                    evidence: LanguageEvidence::Content
                }]
            );
        }
        assert_eq!(manifest.coverage.included, 2);
        for path in ["unrelated.data", "image.xlsm"] {
            assert!(
                manifest
                    .exclusions
                    .iter()
                    .any(|entry| entry.path == path && entry.reason == ExclusionReason::Binary)
            );
        }
        assert_eq!(
            manifest,
            discover_manifest_streaming(&root, &config(), &policy, limits(), &Cancellation::new())
                .unwrap()
        );
    }

    #[test]
    fn office_container_admission_preserves_policy_size_and_aggregate_limits() {
        let temporary = local_tempdir();
        let bytes = b"PK\x03\x04\0opaque package";
        for path in ["first.xlsm", "second.docm"] {
            write_fixture(&temporary, path, bytes);
        }
        let root = fixture_root(&temporary, b"office-container-bounds");
        let policy = DiscoveryPolicy::build(Vec::new(), true).unwrap();
        let exact = u64::try_from(2 * bytes.len()).unwrap();
        let (complete, snapshots) = discover_with_snapshots_at_limit(
            &root,
            &config(),
            &policy,
            limits(),
            BTreeMap::new(),
            exact,
            &Cancellation::new(),
        )
        .unwrap()
        .into_parts();
        assert_eq!(complete.inputs.len(), 2);
        assert!(complete.coverage.complete);
        assert_eq!(
            snapshots.values().map(|s| s.content().len()).sum::<usize>(),
            2 * bytes.len()
        );
        let (partial, snapshots) = discover_with_snapshots_at_limit(
            &root,
            &config(),
            &policy,
            limits(),
            BTreeMap::new(),
            exact - 1,
            &Cancellation::new(),
        )
        .unwrap()
        .into_parts();
        assert!(!partial.coverage.complete);
        assert_eq!(partial.inputs.len(), 1);
        assert_eq!(snapshots.len(), 1);
        assert_eq!(
            partial.coverage.truncation.unwrap().resource,
            DiscoveryTruncationResource::RetainedSourceBytes
        );
        let small =
            DiscoveryLimits::new(100, 16, u64::try_from(bytes.len() - 1).unwrap(), 100).unwrap();
        let oversized =
            discover_manifest_streaming(&root, &config(), &policy, small, &Cancellation::new())
                .unwrap();
        assert!(oversized.inputs.is_empty());
        assert_eq!(oversized.exclusions.len(), 2);
        assert!(
            oversized
                .exclusions
                .iter()
                .all(|e| e.reason == ExclusionReason::Oversized)
        );
        let policy = DiscoveryPolicy::build(
            vec![PolicyRule {
                layer: PolicyLayer::Operation,
                pattern: "*.xlsm".to_owned(),
                source: "operation".to_owned(),
            }],
            true,
        )
        .unwrap();
        let excluded =
            discover_manifest_streaming(&root, &config(), &policy, limits(), &Cancellation::new())
                .unwrap();
        assert_eq!(excluded.inputs.len(), 1);
        assert!(
            excluded
                .exclusions
                .iter()
                .any(|e| e.path == "first.xlsm" && e.reason == ExclusionReason::Policy)
        );
        let cancel = Cancellation::new();
        cancel.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            discover_manifest_streaming(&root, &config(), &policy, limits(), &cancel),
            Err(DiscoveryError::Cancelled(_))
        ));
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
    fn objective_c_content_hints_exclude_comments_and_literals() {
        for source in [
            "/* @interface Example @end */\nint read_item(void);",
            "// @implementation Example\nint read_item(void);",
            "// example \\\n@interface Example\nint read_item(void);",
            "const char *text = \"@interface Example @end\";",
            "const char *text = \"escaped \\\" @implementation Example\";",
            "int marker = '@interface';",
            "const char *text = R\"tag(\" @interface Example)tag\";",
            "const char *text = u8R\"tag(\" @implementation Example)tag\";",
            "const auto *text = uR\"tag(\" @implementation Example)tag\";",
            "const auto *text = UR\"tag(\" @implementation Example)tag\";",
            "const auto *text = LR\"tag(\" @implementation Example)tag\";",
            "// example \\\r\n@interface Example\r\nint read_item(void);",
            "/* #import \"Example.h\" */\nint read_item(void);",
            "const char *text = \"#import <Foundation/Foundation.h>\";",
            "@interfaceSuffix Example;",
            "@implementationSuffix Example;",
            "@interface_Example;",
            "@interfaceÉxample;",
            "/* unfinished @interface Example",
            "\"unfinished @interface Example",
        ] {
            assert_ne!(
                content_language(source.as_bytes()),
                Some("objective-c"),
                "{source}"
            );
        }
        for source in [
            "/* documentation */ @interface Example\n@end",
            "\"literal\"; @implementation Example\n@end",
            "// documentation\n@interface Example\n@end",
            "#import \"Example.h\"",
            "#import <Foundation/Foundation.h>",
            "@interface Example\n@end",
            "R\"tag(\" @interface Hidden)tag\"; @interface Example\n@end",
            "long count = 1'000; @interface Example\n@end",
            "long mask = 0xF'F; @interface Example\n@end",
        ] {
            assert_eq!(
                content_language(source.as_bytes()),
                Some("objective-c"),
                "{source}"
            );
        }
    }

    #[test]
    fn objective_c_content_hints_respect_the_existing_sample_limit() {
        let mut source = vec![b' '; MAX_CLASSIFICATION_BYTES];
        source.extend_from_slice(b"@interface Example\n@end");
        assert_eq!(content_language(&source), None);
        source.splice(..0, b"@interface Example\n@end\n".iter().copied());
        assert_eq!(content_language(&source), Some("objective-c"));
        let mut truncated = vec![b' '; MAX_CLASSIFICATION_BYTES - 1];
        truncated.extend_from_slice("É@interface Example".as_bytes());
        assert_eq!(content_language(&truncated), None);
    }

    #[test]
    fn matlab_content_hints_ignore_literal_and_comment_markers() {
        for source in [
            "/* classdef Phantom\nfunction out = phantom()\n*/\nint read_item(void);",
            "// classdef Phantom\nint read_item(void);",
            "const char *text = \"classdef Phantom\";",
            "text = 'classdef Phantom';",
            "text = \"classdef Phantom\";",
            "% classdef Phantom\n% function out = phantom()\n",
            "%{\nclassdef Phantom\nfunction out = phantom()\n%}\n",
            "object.classdef = 1;",
        ] {
            assert_ne!(
                content_language(source.as_bytes()),
                Some("matlab"),
                "{source}"
            );
        }
        for source in [
            "function\tout = measure(value)\nout = value;\nend\n",
            "classdef\tMeter\nend\n",
            "function ... output\nresult = measure(value)\nresult = value;\nend\n",
            "classdef ... declaration\nMeter\nend\n",
        ] {
            assert_eq!(
                content_language(source.as_bytes()),
                Some("matlab"),
                "{source}"
            );
        }
    }

    #[test]
    fn vba_dialect_evidence_is_bounded_and_requires_a_shared_source_suffix() {
        let source = b"Private Declare PtrSafe Function ReadClock Lib \"clock\" () As LongPtr\n";
        for name in ["module.bas", "class.CLS", "form.frm"] {
            assert_eq!(extension_language(name), None);
            let path = RelativePath::parse(Path::new(name)).unwrap();
            let (_, signals) = classify(&path, source);
            assert_eq!(
                signals,
                vec![LanguageSignal {
                    language: "vba".to_owned(),
                    evidence: LanguageEvidence::Content,
                }]
            );
            let mut outside_sample = vec![b' '; MAX_CLASSIFICATION_BYTES];
            outside_sample.extend_from_slice(source);
            assert!(classify(&path, &outside_sample).1.is_empty());
        }
        for name in ["notes.txt", "archive.vba", "module.bas.bak", "example.rs"] {
            let path = RelativePath::parse(Path::new(name)).unwrap();
            assert!(
                !classify(&path, source)
                    .1
                    .iter()
                    .any(|signal| signal.language == "vba")
            );
        }
    }

    #[test]
    fn classic_export_evidence_is_bounded_and_dialect_evidence_takes_precedence() {
        let header = b"Attribute VB_Name = \"Meter\"\n";
        for name in ["module.bas", "class.CLS", "form.frm"] {
            let path = RelativePath::parse(Path::new(name)).unwrap();
            let expected = vec![LanguageSignal {
                language: "visual-basic-classic".to_owned(),
                evidence: LanguageEvidence::Content,
            }];
            assert_eq!(extension_language(name), None);
            assert_eq!(classify(&path, header).1, expected);
            let mut bounded = vec![b' '; MAX_CLASSIFICATION_BYTES - header.len()];
            bounded.extend_from_slice(header);
            assert_eq!(classify(&path, &bounded).1, expected);
            bounded.splice(0..0, [b' '; 2]);
            assert!(classify(&path, &bounded).1.is_empty());
            let mut dialect = header.to_vec();
            dialect.extend_from_slice(b"Declare PtrSafe Sub Pause Lib \"clock\" ()\n");
            assert_eq!(
                classify(&path, &dialect).1,
                vec![LanguageSignal {
                    language: "vba".to_owned(),
                    evidence: LanguageEvidence::Content,
                }]
            );
        }
        for name in ["notes.txt", "module.bas.bak", "example.rs"] {
            let path = RelativePath::parse(Path::new(name)).unwrap();
            assert!(!classify(&path, header).1.iter().any(|signal| {
                matches!(signal.language.as_str(), "vba" | "visual-basic-classic")
            }));
        }
    }

    #[test]
    fn continued_vba_evidence_requires_a_complete_bounded_literal() {
        let source = b"Declare _\nPtrSafe Function ReadClock Lib _\n\"clock\"";
        let path = RelativePath::parse(Path::new("module.bas")).unwrap();
        let mut bounded = vec![b' '; MAX_CLASSIFICATION_BYTES - source.len()];
        bounded.extend_from_slice(source);
        assert_eq!(
            classify(&path, &bounded).1,
            vec![LanguageSignal {
                language: "vba".to_owned(),
                evidence: LanguageEvidence::Content,
            }]
        );
        bounded.insert(0, b' ');
        assert!(classify(&path, &bounded).1.is_empty());
        let mut continued = b"value = _\n".to_vec();
        continued.extend_from_slice(source);
        assert!(classify(&path, &continued).1.is_empty());
    }

    #[test]
    fn utf16_admission_preserves_snapshots_and_bounded_classification_evidence() {
        let temporary = local_tempdir();
        let text = "' generated file\r\nPrivate Declare PtrSafe Function ReadClock Lib \"clock\" () As LongPtr\r\n";
        let bytes = std::iter::once(0xfeff_u16)
            .chain(text.encode_utf16())
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        write_fixture(&temporary, "module.bas", &bytes);
        write_fixture(&temporary, "malformed.bas", &[0xff, 0xfe, 0, 0xd8]);
        let config = ConfigSnapshot::resolve(&[]).unwrap();
        let root = fixture_root(&temporary, b"utf16-admission");
        let policy = DiscoveryPolicy::build(Vec::new(), true).unwrap();
        let result = discover_with_snapshots(
            &root,
            &config,
            &policy,
            DiscoveryLimits::from_config(&config),
            BTreeMap::new(),
            &Cancellation::new(),
        )
        .unwrap();
        assert_eq!(result.manifest.inputs.len(), 1);
        let input = &result.manifest.inputs[0];
        assert_eq!(input.path, "module.bas");
        assert_eq!(input.encoding, "utf-16le");
        assert_eq!(input.class, InputClass::Generated);
        assert!(
            input
                .language_signals
                .iter()
                .any(|signal| signal.language == "vba"
                    && signal.evidence == LanguageEvidence::Content)
        );
        assert_eq!(input.content_hash, content_hash(&bytes));
        assert_eq!(input.bytes, u64::try_from(bytes.len()).unwrap());
        assert_eq!(result.snapshots.get(&input.file).unwrap().content(), bytes);
        assert_eq!(result.manifest.exclusions.len(), 1);
        assert_eq!(
            result.manifest.exclusions[0].reason,
            ExclusionReason::Binary
        );
        let encoded = serde_json::to_value(input).unwrap();
        let roundtrip: ManifestInput = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(roundtrip, *input);
        let mut legacy = encoded;
        legacy.as_object_mut().unwrap().remove("encoding");
        let legacy: ManifestInput = serde_json::from_value(legacy).unwrap();
        assert_eq!(legacy.encoding, "utf-8");
    }

    #[test]
    fn configured_source_languages_preserve_detection_and_binary_exclusions() {
        let temporary = local_tempdir();
        write_fixture(&temporary, "module.bas", b"Sub Visit()\nEnd Sub\n");
        write_fixture(&temporary, "other.rs", b"fn visit() {}\n");
        write_fixture(&temporary, "binary.bas", b"\0binary");
        let contents = "version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.extensions]\nbas='vba'\n[extensions.source-languages.data.paths]\n'other.rs'='unsupported-language'\n";
        let config = ConfigSnapshot::resolve(&[rootlight_config::ConfigLayer {
            source: rootlight_config::ConfigSource::User,
            contents,
        }])
        .unwrap();
        let root = fixture_root(&temporary, b"configured-languages");
        let policy = DiscoveryPolicy::build(Vec::new(), true).unwrap();
        let manifest = discover(
            &root,
            &config,
            &policy,
            DiscoveryLimits::from_config(&config),
            &Cancellation::new(),
        )
        .unwrap();
        assert_eq!(manifest.inputs.len(), 2);
        assert!(
            manifest
                .exclusions
                .iter()
                .any(|entry| entry.path == "binary.bas" && entry.reason == ExclusionReason::Binary)
        );
        let basic = manifest
            .inputs
            .iter()
            .find(|input| input.path == "module.bas")
            .unwrap();
        assert_eq!(
            basic.language_signals,
            vec![LanguageSignal {
                language: "vba".to_owned(),
                evidence: LanguageEvidence::Configuration
            }]
        );
        let rust = manifest
            .inputs
            .iter()
            .find(|input| input.path == "other.rs")
            .unwrap();
        assert!(
            rust.language_signals
                .iter()
                .any(|signal| signal.language == "rust"
                    && signal.evidence == LanguageEvidence::Extension)
        );
        assert!(
            rust.language_signals
                .iter()
                .any(|signal| signal.language == "unsupported-language"
                    && signal.evidence == LanguageEvidence::Configuration)
        );
        assert_eq!(manifest.configuration_hash, config.hash());
    }

    #[test]
    fn cobol_formats_require_configuration_without_suffix_inference() {
        let generic = language_capabilities()
            .iter()
            .find(|capability| capability.language == "cobol")
            .unwrap();
        assert_eq!(generic.suffixes, [".cbl", ".cob"]);
        assert_eq!(generic.analyzers, ["source-fallback"]);
        for language in ["cobol-fixed", "cobol-free"] {
            let capability = language_capabilities()
                .iter()
                .find(|capability| capability.language == language)
                .unwrap();
            assert!(capability.suffixes.is_empty());
            assert!(capability.aliases.is_empty());
            assert_eq!(capability.detectors, ["configuration"]);
            assert_eq!(capability.analyzers, ["treesitter"]);
            assert_eq!(capability.maximum_tier, "tier_d");
            assert_eq!(canonical_language(language), Some(language));
        }
        assert_eq!(canonical_language("cobol"), Some("cobol"));
        let source = b"       IDENTIFICATION DIVISION.\n       PROGRAM-ID. SAMPLE.\n";
        for path in [
            "sample.cbl",
            "src/SAMPLE.CBL",
            "sample.cob",
            "src/SAMPLE.COB",
        ] {
            assert_eq!(extension_language(path), Some("cobol"));
            let (class, signals) = classify(&RelativePath::parse(Path::new(path)).unwrap(), source);
            assert_eq!(class, InputClass::Source);
            assert_eq!(
                signals,
                vec![LanguageSignal {
                    language: "cobol".to_owned(),
                    evidence: LanguageEvidence::Extension,
                }],
                "{path}"
            );
        }
        for path in ["sample.cbl.bak", "sample.cob.txt", "sample.cobol-fixed"] {
            assert_eq!(extension_language(path), None);
        }
    }

    #[test]
    fn configured_cobol_formats_preserve_detection_and_binary_exclusions() {
        for newline in ["\n", "\r\n"] {
            let temporary = local_tempdir();
            let source = "       IDENTIFICATION DIVISION.\n       PROGRAM-ID. SAMPLE.\n"
                .replace('\n', newline);
            for path in ["fixed.cbl", "free.cob", "generic.cbl"] {
                write_fixture(&temporary, path, source.as_bytes());
            }
            write_fixture(&temporary, "binary.cbl", b"\0binary");
            let config = ConfigSnapshot::resolve(&[rootlight_config::ConfigLayer {
                source: rootlight_config::ConfigSource::User,
                contents: "version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.extensions]\ncbl='cobol-fixed'\n[extensions.source-languages.data.paths]\n'free.cob'='cobol-free'\n'generic.cbl'='cobol'\n",
            }])
            .unwrap();
            let root = fixture_root(&temporary, b"configured-cobol");
            let manifest = discover(
                &root,
                &config,
                &DiscoveryPolicy::build(Vec::new(), true).unwrap(),
                DiscoveryLimits::from_config(&config),
                &Cancellation::new(),
            )
            .unwrap();
            assert_eq!(manifest.configuration_hash, config.hash());
            assert_eq!(manifest.inputs.len(), 3);
            assert!(manifest.exclusions.iter().any(|entry| {
                entry.path == "binary.cbl" && entry.reason == ExclusionReason::Binary
            }));
            for (path, configured) in [
                ("fixed.cbl", "cobol-fixed"),
                ("free.cob", "cobol-free"),
                ("generic.cbl", "cobol"),
            ] {
                let input = manifest
                    .inputs
                    .iter()
                    .find(|input| input.path == path)
                    .unwrap();
                assert_eq!(input.class, InputClass::Source);
                assert_eq!(input.bytes, u64::try_from(source.len()).unwrap());
                assert_eq!(input.content_hash, content_hash(source.as_bytes()));
                let configuration: Vec<_> = input
                    .language_signals
                    .iter()
                    .filter(|signal| signal.evidence == LanguageEvidence::Configuration)
                    .collect();
                assert_eq!(configuration.len(), 1);
                assert_eq!(configuration[0].language, configured);
                assert!(input.language_signals.iter().any(|signal| {
                    signal.language == "cobol" && signal.evidence == LanguageEvidence::Extension
                }));
            }
        }
    }

    #[test]
    fn matlab_scripts_have_content_evidence_without_function_declarations() {
        let path = RelativePath::parse(Path::new("measure.m")).unwrap();
        for source in [
            "% Measurements\nvalue = 1;\nresult = value + 1;\n",
            "% @interface Phantom\nvalue = 1;\n",
            "%{\n@interface Phantom\n%}\nvalue = 1;\n",
            "for index = 1:3\nvalue(index) = index;\nend\n",
            "values = input .* weights;\n",
            "values = 1.*weights;\n",
            "values = [1 2; 3 4];\n",
            "[first, second] = measure();\nresult = first + second;\n",
            "[value, index, status] = inspect(input);\n",
            "value = 1;\n% Measurement\nresult = value;\n",
            "\u{feff}%% Measurements\r\nvalue = 1;\r\n",
        ] {
            let (_, signals) = classify(&path, source.as_bytes());
            let content: Vec<_> = signals
                .iter()
                .filter(|signal| signal.evidence == LanguageEvidence::Content)
                .map(|signal| signal.language.as_str())
                .collect();
            assert_eq!(content, ["matlab"], "{source}");
        }
    }

    #[test]
    fn ambiguous_m_sources_inherit_only_unanimous_peer_evidence() {
        let temporary = local_tempdir();
        write_fixture(&temporary, "startup.m", b"addpath(genpath(pwd));\n");
        write_fixture(
            &temporary,
            "measure.m",
            b"function result = measure(value)\nresult = value;\nend\n",
        );
        let root = fixture_root(&temporary, b"shared-m-context");
        let policy = DiscoveryPolicy::build(Vec::new(), true).unwrap();
        let discover = || {
            discover_manifest_streaming(&root, &config(), &policy, limits(), &Cancellation::new())
                .unwrap()
        };
        let signals = |manifest: &DiscoveryManifest| {
            manifest
                .inputs
                .iter()
                .find(|input| input.path == "startup.m")
                .unwrap()
                .language_signals
                .clone()
        };
        assert!(
            !signals(&discover())
                .iter()
                .any(|signal| signal.evidence == LanguageEvidence::Context)
        );

        write_fixture(
            &temporary,
            "estimate.m",
            b"function result = estimate(value)\nresult = value;\nend\n",
        );
        assert!(signals(&discover()).iter().any(|signal| {
            signal.evidence == LanguageEvidence::Context && signal.language == "matlab"
        }));

        write_fixture(&temporary, "estimate.m", b"@interface Estimate\n@end\n");
        assert!(
            !signals(&discover())
                .iter()
                .any(|signal| signal.evidence == LanguageEvidence::Context)
        );

        write_fixture(&temporary, "measure.m", b"@interface Measure\n@end\n");
        assert!(signals(&discover()).iter().any(|signal| {
            signal.evidence == LanguageEvidence::Context && signal.language == "objective-c"
        }));
    }

    fn assert_encoded_shared_m_context(little_endian: bool) {
        let temporary = local_tempdir();
        let encoding = if little_endian {
            "utf-16le"
        } else {
            "utf-16be"
        };
        let encode = |source: &str| {
            std::iter::once(0xfeff_u16)
                .chain(source.encode_utf16())
                .flat_map(|unit| {
                    if little_endian {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    }
                })
                .collect::<Vec<_>>()
        };
        let startup = encode("addpath(genpath(pwd));\r\n");
        let measure = encode("function result = measure(value)\r\nresult = value;\r\nend\r\n");
        let estimate = encode("function result = estimate(value)\r\nresult = value;\r\nend\r\n");
        let objc_measure = encode("@interface Measure\r\n@end\r\n");
        let objc_estimate = encode("@interface Estimate\r\n@end\r\n");
        write_fixture(&temporary, "startup.m", &startup);
        write_fixture(&temporary, "measure.m", &measure);
        let root = fixture_root(&temporary, b"encoded-shared-m-context");
        let config = config();
        let policy = DiscoveryPolicy::build(Vec::new(), true).unwrap();
        let inspect = |measure: &[u8], estimate: Option<&[u8]>, context: Option<&str>| {
            let result = discover_with_snapshots(
                &root,
                &config,
                &policy,
                limits(),
                BTreeMap::new(),
                &Cancellation::new(),
            )
            .unwrap();
            assert!(result.manifest.exclusions.is_empty());
            assert_eq!(
                result.manifest.inputs.len(),
                2 + usize::from(estimate.is_some())
            );
            for (path, bytes) in [("startup.m", startup.as_slice()), ("measure.m", measure)]
                .into_iter()
                .chain(estimate.map(|bytes| ("estimate.m", bytes)))
            {
                let input = result
                    .manifest
                    .inputs
                    .iter()
                    .find(|input| input.path == path)
                    .unwrap();
                assert_eq!(input.encoding, encoding);
                assert_eq!(input.content_hash, content_hash(bytes));
                assert_eq!(input.bytes, u64::try_from(bytes.len()).unwrap());
                assert_eq!(result.snapshots.get(&input.file).unwrap().content(), bytes);
            }
            let input = result
                .manifest
                .inputs
                .iter()
                .find(|input| input.path == "startup.m")
                .unwrap();
            let actual: Vec<_> = input
                .language_signals
                .iter()
                .filter(|signal| signal.evidence == LanguageEvidence::Context)
                .map(|signal| signal.language.as_str())
                .collect();
            assert_eq!(
                actual,
                context.into_iter().collect::<Vec<_>>(),
                "{encoding}"
            );
            input.file
        };
        let file = inspect(&measure, None, None);
        write_fixture(&temporary, "estimate.m", &estimate);
        assert_eq!(inspect(&measure, Some(&estimate), Some("matlab")), file);
        write_fixture(&temporary, "estimate.m", &objc_estimate);
        assert_eq!(inspect(&measure, Some(&objc_estimate), None), file);
        write_fixture(&temporary, "measure.m", &objc_measure);
        assert_eq!(
            inspect(&objc_measure, Some(&objc_estimate), Some("objective-c")),
            file
        );
    }

    #[test]
    fn utf16le_shared_m_context_tracks_peer_language_changes() {
        assert_encoded_shared_m_context(true);
    }

    #[test]
    fn utf16be_shared_m_context_tracks_peer_language_changes() {
        assert_encoded_shared_m_context(false);
    }

    #[test]
    fn matlab_script_hints_do_not_override_objc_or_known_suffixes() {
        for source in [
            "/* % example\nclassdef Phantom\n*/\n@interface Meter\n@end\n",
            "const char *text = \"escaped \\\";\\\nfunction phantom()\";\n@interface Meter\n@end\n",
            "#import \"Meter.h\"\n@implementation Meter\n@end\n",
            "@interface Meter\n@end\n",
            "@interface/* header */Meter\n@end\n",
        ] {
            let (_, signals) = classify(
                &RelativePath::parse(Path::new("meter.m")).unwrap(),
                source.as_bytes(),
            );
            let content: Vec<_> = signals
                .iter()
                .filter(|signal| signal.evidence == LanguageEvidence::Content)
                .map(|signal| signal.language.as_str())
                .collect();
            assert_eq!(content, ["objective-c"], "{source}");
        }
        for source in [
            "int values[] = {1, 2};\nint result = values[1];\n",
            "int value = 10\n% 3;\n",
            "result = [object read:value];\n",
            "[first, second] == measure();\n",
            "[first, second] =\n",
            "text = '[first, second] = measure()';\n",
            "// [first, second] = measure();\n",
            "text = 'can''t; classdef Phantom';\n",
            "const char *text = R\"tag(\nfunction phantom()\n)tag\";\n",
            "... function phantom()\nvalue = 1;\n",
        ] {
            assert_ne!(
                content_language_with_scripts(source.as_bytes(), true),
                Some("matlab"),
                "{source}"
            );
        }
        for path in ["measure.c", "measure.mm", "measure.txt"] {
            let (_, signals) = classify(
                &RelativePath::parse(Path::new(path)).unwrap(),
                b"% example\nvalue = 1;\n",
            );
            assert!(
                !signals.iter().any(|signal| signal.language == "matlab"),
                "{path}"
            );
        }
    }

    #[test]
    fn matlab_block_comment_nesting_does_not_expose_fake_code() {
        let source = b"%{\r\n%{\r\n%}\r\nclassdef Phantom\r\n%}\r\nvalue = 1;\r\n";
        assert_eq!(content_language(source), None);
        assert_eq!(content_language_with_scripts(source, true), Some("matlab"));
        assert_eq!(
            content_language(b"%{ inline text\nfunction out = actual()\nout = 1;\nend\n"),
            Some("matlab")
        );
        assert_eq!(
            content_language(b"%{\n%} trailing text\nclassdef Phantom\n"),
            None
        );
    }

    #[test]
    fn matlab_hints_respect_the_existing_byte_prefix_bound() {
        let mut source = vec![b' '; MAX_CLASSIFICATION_BYTES];
        source.extend_from_slice(b"function out = measure(value)\nend\n");
        assert_eq!(content_language_with_scripts(&source, true), None);
        for byte in 0..=u8::MAX {
            let mut source = vec![byte; MAX_CLASSIFICATION_BYTES - 1];
            source.extend_from_slice(b"\xf0\x9f\x98\x80 classdef Phantom");
            let _ = content_language_with_scripts(&source, true);
        }
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
    fn python_content_hints_reject_initializers_and_non_function_names() {
        for source in [
            "def value = factory(option: false)\n",
            "def value = factory(option: false, nested: create())\n",
            "def value = 'factory(option: false)'\n",
            "def value // factory(option: false)\n",
            "def value /* factory(option: false) */\n",
            "def owner.read(value):\n",
            "def Result read(value):\n",
            "def 12read(value):\n",
            "# def read(value):\n",
            "// def read(value):\n",
            "text = 'def read(value):'\n",
        ] {
            assert_eq!(content_language(source.as_bytes()), None, "{source}");
            let path = RelativePath::parse(Path::new("script.unknown")).unwrap();
            let (_, signals) = classify(&path, source.as_bytes());
            assert!(signals.is_empty(), "{source}: {signals:?}");
        }
    }

    #[test]
    fn python_content_hints_preserve_function_signature_forms() {
        for source in [
            "def read(value):\n    return value\n",
            "async def read(value: str) -> str:\n    return value\n",
            "    def _read (value = factory(option=False)): return value\n",
            "@decorator\ndef read(value: 'Item') -> 'Item': return value\n",
            "def read[T: Item](value: T) -> T: return value\n",
            "def read[T: tuple[Item, ...]](value: T): return value\n",
            "def λ(value): return value\n",
            "def e\u{301}(value): return value\n",
        ] {
            assert_eq!(
                content_language(source.as_bytes()),
                Some("python"),
                "{source}"
            );
        }
    }

    #[test]
    fn groovy_script_suffixes_preserve_exact_extension_evidence() {
        for path in [
            "task.groovy",
            "task.gvy",
            "task.gy",
            "task.gsh",
            "build.gradle",
            "flow.jenkins",
            "scripts/TASK.GVY",
        ] {
            let path = RelativePath::parse(Path::new(path)).unwrap();
            assert_eq!(
                classify(&path, b"def values = new String[0]\n").1,
                vec![LanguageSignal {
                    language: "groovy".to_owned(),
                    evidence: LanguageEvidence::Extension,
                }]
            );
        }
        for path in [
            "task.gvy.bak",
            "task.gsh.txt",
            "flow.jenkins.bak",
            "build.gradle.kts",
            "task.gtpl",
            "task.grt",
        ] {
            assert_ne!(extension_language(path), Some("groovy"), "{path}");
        }
    }

    #[test]
    fn groovy_shebangs_use_interpreter_identity_not_arguments() {
        for header in [
            "#!/usr/bin/groovy",
            "#!/usr/bin/env groovy",
            "#!/usr/bin/env -S groovy -Dmode=plain",
            "#!/usr/bin/env -S -u UNUSED groovy",
        ] {
            assert_eq!(
                shebang_language(format!("{header}\ndef run() {{}}\n").as_bytes()),
                Some("groovy"),
                "{header}"
            );
        }
        for header in [
            "#!/usr/bin/groovyc",
            "#!/usr/bin/groovysh",
            "#!/usr/bin/env GROOVY=groovy",
            "#!/usr/bin/env -S ${INTERPRETER} groovy",
            "#!/usr/bin/env groovy -x",
        ] {
            assert_ne!(
                shebang_language(header.as_bytes()),
                Some("groovy"),
                "{header}"
            );
        }
        assert_eq!(
            shebang_language(b"#!/usr/bin/python groovy\n"),
            Some("python")
        );
    }

    #[test]
    fn groovy_known_filenames_have_distinct_filename_evidence() {
        assert_eq!(filename_language("ci\\Jenkinsfile"), Some("groovy"));
        for name in ["Jenkinsfile", "ci/Jenkinsfile", "ci/JENKINSFILE"] {
            let path = RelativePath::parse(Path::new(name)).unwrap();
            let signals = classify(&path, b"node { echo 'ready' }\n").1;
            assert_eq!(
                serde_json::to_value(signals).unwrap(),
                serde_json::json!([
                    {"language": "groovy", "evidence": "filename"}
                ])
            );
            assert_eq!(extension_language(name), None);
        }
        for name in [
            "NotJenkinsfile",
            "Jenkinsfile.bak",
            "Jenkinsfile.txt",
            "Jenkinsfile/notes",
            "ci/Jenkinsfile.py",
        ] {
            let path = RelativePath::parse(Path::new(name)).unwrap();
            assert!(
                !classify(&path, b"node { echo 'ready' }\n")
                    .1
                    .iter()
                    .any(|signal| signal.language == "groovy"),
                "{name}"
            );
        }
    }

    #[test]
    fn language_evidence_preserves_existing_wire_labels() {
        for (label, evidence) in [
            ("extension", LanguageEvidence::Extension),
            ("shebang", LanguageEvidence::Shebang),
            ("manifest", LanguageEvidence::Manifest),
            ("content", LanguageEvidence::Content),
            ("context", LanguageEvidence::Context),
            ("filename", LanguageEvidence::Filename),
            ("configuration", LanguageEvidence::Configuration),
            ("project", LanguageEvidence::Project),
        ] {
            let value = serde_json::json!(label);
            assert_eq!(serde_json::to_value(evidence).unwrap(), value);
            assert_eq!(
                serde_json::from_value::<LanguageEvidence>(value).unwrap(),
                evidence
            );
        }
    }

    #[test]
    fn groovy_capability_advertises_native_structural_analysis() {
        let capability = language_capabilities()
            .iter()
            .find(|entry| entry.language == "groovy")
            .unwrap();
        assert_eq!(capability.analyzers, ["treesitter"]);
        assert_eq!(capability.maximum_tier, "tier_d");
        assert_eq!(capability.detectors, ["extension", "filename", "shebang"]);
    }

    #[test]
    fn haskell_source_suffixes_advertise_native_structural_analysis() {
        let capability = language_capabilities()
            .iter()
            .find(|entry| entry.language == "haskell")
            .expect("Haskell capability is declared");
        assert_eq!(capability.suffixes, [".hs"]);
        assert!(capability.aliases.is_empty());
        assert_eq!(capability.detectors, ["extension"]);
        assert_eq!(capability.analyzers, ["treesitter"]);
        assert_eq!(capability.maximum_tier, "tier_d");
        assert_eq!(canonical_language("haskell"), Some("haskell"));
        for path in ["Main.hs", "src/Library.hs", "src/LIBRARY.HS"] {
            assert_eq!(extension_language(path), Some("haskell"), "{path}");
            let (class, signals) = classify(
                &RelativePath::parse(Path::new(path)).expect("fixture path is valid"),
                b"module Example where\nvalue = 1\n",
            );
            assert_eq!(class, InputClass::Source);
            assert_eq!(
                signals,
                vec![LanguageSignal {
                    language: "haskell".to_owned(),
                    evidence: LanguageEvidence::Extension,
                }],
                "{path}"
            );
        }
        for path in [
            "Main.lhs",
            "Main.hsc",
            "Main.hs.bak",
            "Main.hs.txt",
            "Main.hi",
            "Main.o",
        ] {
            assert_eq!(extension_language(path), None, "{path}");
            let (_, signals) = classify(
                &RelativePath::parse(Path::new(path)).expect("fixture path is valid"),
                b"module Example where\nvalue = 1\n",
            );
            assert!(signals.is_empty(), "{path}: {signals:?}");
        }
    }

    #[test]
    fn fsharp_native_probe_is_eligibility_not_positive_routing() {
        let path = RelativePath::parse(Path::new("Source.fs")).unwrap();
        for source in [
            b"let identity value = value\n".as_slice(),
            b"let map = fun value -> value\n",
            b"type Alias = System.String\n",
            b"type Choice = First | Second\n",
        ] {
            assert!(fsharp_native_probe_allowed(source, &Cancellation::new()).unwrap());
            assert!(classify(&path, source).1.is_empty());
        }
        for source in [
            b": square dup * ;\n".as_slice(),
            b"let identity value = value\n: square dup * ;\n",
            b"let identity value = value\n(* unfinished",
            b"let identity value = \"unfinished",
        ] {
            assert!(!fsharp_native_probe_allowed(source, &Cancellation::new()).unwrap());
        }
    }

    #[test]
    fn fsharp_native_probe_reuses_utf16_validation_and_both_sample_budgets() {
        let encoded = |source: &str, little_endian: bool| -> Vec<u8> {
            std::iter::once(0xfeff_u16)
                .chain(source.encode_utf16())
                .flat_map(|unit| {
                    if little_endian {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    }
                })
                .collect()
        };
        for little_endian in [true, false] {
            for source in [
                "let identity value = value\n",
                "let map = fun value -> value\n",
                "type Choice = First | Second\n",
            ] {
                assert!(
                    fsharp_native_probe_allowed(
                        &encoded(source, little_endian),
                        &Cancellation::new()
                    )
                    .unwrap()
                );
            }
            assert!(
                !fsharp_native_probe_allowed(
                    &encoded(
                        "let map = fun value -> value\n: square dup * ;\n",
                        little_endian
                    ),
                    &Cancellation::new(),
                )
                .unwrap()
            );
            let original_bound = (MAX_CLASSIFICATION_BYTES - 2) / 2;
            let mut source = "let identity value = value\n".to_owned();
            source.push_str(&" ".repeat(original_bound - source.len() - 1));
            let safe_prefix = source.clone();
            source.push_str("\"closed outside the sample\"");
            assert!(
                !fsharp_native_probe_allowed(
                    &encoded(&source, little_endian),
                    &Cancellation::new()
                )
                .unwrap()
            );
            source = format!("{safe_prefix} \nlet later = 1\n");
            let mut bytes = encoded(&source, little_endian);
            assert!(fsharp_native_probe_allowed(&bytes, &Cancellation::new()).unwrap());
            bytes.extend_from_slice(&if little_endian {
                0xd800_u16.to_le_bytes()
            } else {
                0xd800_u16.to_be_bytes()
            });
            assert!(!fsharp_native_probe_allowed(&bytes, &Cancellation::new()).unwrap());

            let header = "let value = ";
            let wide = "界".repeat((MAX_CLASSIFICATION_BYTES - header.len() - 1) / 3);
            let source = format!("{header}{wide}\"closed outside the sample\"");
            assert!(encoded(&source, little_endian).len() < MAX_CLASSIFICATION_BYTES);
            assert!(
                !fsharp_native_probe_allowed(
                    &encoded(&source, little_endian),
                    &Cancellation::new()
                )
                .unwrap()
            );
        }
        for source in [
            vec![0xff],
            vec![0xff, 0xfe, 0x61],
            vec![0xff, 0xfe, 0, 0xd8],
            vec![0xfe, 0xff, 0xdc, 0],
            b"let value = 1\0".to_vec(),
        ] {
            assert!(!fsharp_native_probe_allowed(&source, &Cancellation::new()).unwrap());
        }
    }

    #[test]
    fn fsharp_native_probe_checks_cancellation_before_admission() {
        let cancellation = Cancellation::new();
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        for source in [
            b"let identity value = value\n".as_slice(),
            b"",
            &[0xff, 0xfe, 0, 0xd8],
        ] {
            assert!(matches!(
                fsharp_native_probe_allowed(source, &cancellation),
                Err(DiscoveryError::Cancelled(cancelled))
                    if cancelled.reason() == CancellationReason::ClientRequest
            ));
        }
    }

    #[test]
    fn fsharp_shared_suffix_uses_bounded_content_without_suffix_or_peer_promotion() {
        let source = b"module Example.Core\nlet value = 1\n";
        for name in ["Library.fs", "src/LIBRARY.FS"] {
            let path = RelativePath::parse(Path::new(name)).unwrap();
            assert_eq!(extension_language(name), None);
            assert_eq!(
                classify(&path, source).1,
                vec![LanguageSignal {
                    language: "fsharp".to_owned(),
                    evidence: LanguageEvidence::Content,
                }]
            );
        }
        for name in ["Library.fs.bak", "Library.txt", ".fs", "dir/.fs"] {
            let path = RelativePath::parse(Path::new(name)).unwrap();
            assert!(classify(&path, source).1.is_empty(), "{name}");
        }
        for (name, language) in [
            ("Library.fsx", "fsharp"),
            ("Library.fsi", "fsharp-signature"),
        ] {
            let path = RelativePath::parse(Path::new(name)).unwrap();
            assert_eq!(
                classify(&path, source).1,
                vec![LanguageSignal {
                    language: language.to_owned(),
                    evidence: LanguageEvidence::Extension,
                }]
            );
        }
        let path = RelativePath::parse(Path::new("Library.fs")).unwrap();
        for source in [
            b": square dup * ;\n".as_slice(),
            b"// module Example\n// let value = 1\n",
            b"module Example\nlet value = 1\n: square dup * ;\n",
            b"module Example\nlet value = 1 : square dup * ;\n",
            b"module Example\nlet value = 1\n0 [if] : square dup * ; [then]\n",
            b"module Example\nlet value =\n",
            b"",
        ] {
            assert!(classify(&path, source).1.is_empty());
        }
        let mut outside = vec![b' '; MAX_CLASSIFICATION_BYTES];
        outside.extend_from_slice(source);
        assert!(classify(&path, &outside).1.is_empty());
    }

    #[test]
    fn fsharp_implicit_modules_supply_only_content_evidence_on_the_shared_suffix() {
        for source in [
            "open Example.Support\nlet answer = 1\n",
            "[<EntryPoint>]\nlet main _ = 0\n",
            "let identity (value: int) : int = value\n",
            "type Box<'T> = Box of 'T\n",
            "let answer = 1\nlet identity (value: int) = value\n",
        ] {
            for name in ["Source.fs", "src/SOURCE.FS"] {
                let path = RelativePath::parse(Path::new(name)).unwrap();
                assert_eq!(extension_language(name), None);
                assert_eq!(
                    classify(&path, source.as_bytes()).1,
                    vec![LanguageSignal {
                        language: "fsharp".to_owned(),
                        evidence: LanguageEvidence::Content,
                    }],
                    "{name}: {source:?}"
                );
                let mixed = format!("{source}: square dup * ;\n");
                assert!(classify(&path, mixed.as_bytes()).1.is_empty());
            }
            let path = RelativePath::parse(Path::new("Source.txt")).unwrap();
            assert!(classify(&path, source.as_bytes()).1.is_empty());
        }
        let path = RelativePath::parse(Path::new("Source.fs")).unwrap();
        assert!(classify(&path, b"let answer = 1\n").1.is_empty());
    }

    #[test]
    fn fsharp_shared_suffix_accepts_only_module_accessibility_source_evidence() {
        for accessibility in ["private", "internal", "public"] {
            for recursive in ["", "rec "] {
                let source =
                    format!("module {accessibility} {recursive}Example.Core\nlet value = 1\n");
                for name in ["Library.fs", "src/LIBRARY.FS"] {
                    let path = RelativePath::parse(Path::new(name)).unwrap();
                    assert_eq!(extension_language(name), None);
                    assert_eq!(
                        classify(&path, source.as_bytes()).1,
                        vec![LanguageSignal {
                            language: "fsharp".to_owned(),
                            evidence: LanguageEvidence::Content,
                        }],
                        "{name}: {source:?}"
                    );
                }
                let path = RelativePath::parse(Path::new("Library.txt")).unwrap();
                assert!(classify(&path, source.as_bytes()).1.is_empty());
            }
        }
        let path = RelativePath::parse(Path::new("Library.fs")).unwrap();
        for source in [
            "namespace internal Example\nlet value = 1\n",
            "module public private Example\nlet value = 1\n",
            "module internal Example\nlet value =\n",
            "module internal Example\nlet value = 1\n: square dup * ;\n",
            "// module internal Example\n// let value = 1\n",
        ] {
            assert!(
                classify(&path, source.as_bytes()).1.is_empty(),
                "{source:?}"
            );
        }
        let mut outside = vec![b' '; MAX_CLASSIFICATION_BYTES];
        outside.extend_from_slice(b"module internal Example\nlet value = 1\n");
        assert!(classify(&path, &outside).1.is_empty());
    }

    #[test]
    fn fsharp_shared_suffix_retains_quote_character_source_evidence() {
        for literal in ["'\"'", "'\\\"'"] {
            for newline in ["\n", "\r\n"] {
                let source = format!("module Example{newline}let quote = {literal}{newline}");
                let path = RelativePath::parse(Path::new("Library.fs")).unwrap();
                let expected = vec![LanguageSignal {
                    language: "fsharp".to_owned(),
                    evidence: LanguageEvidence::Content,
                }];
                assert_eq!(extension_language("Library.fs"), None);
                assert_eq!(classify(&path, source.as_bytes()).1, expected);
                let mut sampled = source.as_bytes().to_vec();
                sampled.resize(MAX_CLASSIFICATION_BYTES, b' ');
                sampled.extend_from_slice(b"let following = 1\n");
                assert_eq!(classify(&path, &sampled).1, expected);
                let text = RelativePath::parse(Path::new("Library.txt")).unwrap();
                assert!(classify(&text, source.as_bytes()).1.is_empty());
                let mixed = format!("{source}: square dup * ;{newline}");
                assert!(classify(&path, mixed.as_bytes()).1.is_empty());
            }
        }
    }

    #[test]
    fn fsharp_multiline_shared_suffix_retains_only_source_shaped_evidence() {
        let path = RelativePath::parse(Path::new("Library.fs")).unwrap();
        for source in [
            "module Example\nlet value =\n    1",
            "namespace Example\ntype Entry =\n    { Count: int }\n",
            "module Example =\n    let add left right =\n        left + right\n",
            "module Example\nmodule Inner =\n    type Choice =\n        | Empty\n",
        ] {
            assert_eq!(
                classify(&path, source.as_bytes()).1,
                vec![LanguageSignal {
                    language: "fsharp".to_owned(),
                    evidence: LanguageEvidence::Content,
                }],
                "{source:?}"
            );
            let text_path = RelativePath::parse(Path::new("Library.txt")).unwrap();
            assert!(classify(&text_path, source.as_bytes()).1.is_empty());
        }
        for source in [
            "module Example\nlet value =\n1\n",
            "module Example\nlet value =\n    = 1\n",
            "module Example\nlet value =\n    // no expression\n",
            "module Example\nlet value =\n    : square dup * ;\n",
            "let value =\n    1\n",
        ] {
            assert!(
                classify(&path, source.as_bytes()).1.is_empty(),
                "{source:?}"
            );
        }
    }

    #[test]
    fn fsharp_decoded_prefix_does_not_promote_partial_final_lines() {
        fn encode(source: &str, little_endian: bool) -> Vec<u8> {
            std::iter::once(0xfeff_u16)
                .chain(source.encode_utf16())
                .flat_map(|unit| {
                    if little_endian {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    }
                })
                .collect()
        }

        fn original_byte_limited(prefix: &str, tail: &str) -> String {
            let mut source = prefix.to_owned();
            let decoded_limit = (MAX_CLASSIFICATION_BYTES - 2) / 2;
            source.push_str(&" ".repeat(decoded_limit - source.len() - tail.len()));
            source.push_str(tail);
            source
        }

        fn decoded_byte_limited(prefix: &str, tail: &str) -> String {
            let mut source = format!("{prefix}//");
            let filler_end = MAX_CLASSIFICATION_BYTES - 1 - 1 - tail.len();
            source.push_str(&"界".repeat((filler_end - source.len()) / 3));
            source.push_str(&" ".repeat(filler_end - source.len()));
            source.push('\n');
            source.push_str(tail);
            source
        }

        let original_tail = original_byte_limited("module Example\n", "let value = 1");
        let original_rhs = original_byte_limited("module Example\nlet value =\n", "1");
        let prior_evidence =
            original_byte_limited("module Example\nlet prior = 1\n", "let value = 1");
        let decoded_tail = decoded_byte_limited("module Example\n", "let value = variable");
        let decoded_prior =
            decoded_byte_limited("module Example\nlet prior = 1\n", "let value = variable");
        let mut scalar_tail = original_byte_limited("module Example\n", "let value = variable");
        scalar_tail.remove("module Example\n".len());
        let mut cases = vec![
            ("OriginalTail.fs", format!("{original_tail} + 2\n"), false),
            ("OriginalRhs.fs", format!("{original_rhs} + 2\n"), false),
            ("OriginalScalar.fs", format!("{scalar_tail}𝑥\n"), false),
            ("DecodedTail.fs", format!("{decoded_tail}λ\n"), false),
            ("PriorEvidence.fs", format!("{prior_evidence} + 2\n"), true),
            ("DecodedPrior.fs", format!("{decoded_prior}λ\n"), true),
            ("KnownEof.fs", original_tail, true),
            ("KnownRhsEof.fs", original_rhs, true),
            (
                "Multiline.fs",
                "module Example\r\nlet value =\r\n    1".to_owned(),
                true,
            ),
            ("Headerless.fs", "let value =\n    1\n".to_owned(), false),
            (
                "Forth.fs",
                "module Example\nlet value =\n    : square dup * ;\n".to_owned(),
                false,
            ),
        ];
        for (path, tail, remainder) in [
            ("PriorString.fs", "let next = \"unfinished", "\"\n"),
            ("PriorComment.fs", "(* unfinished", " *)\n"),
            ("PriorName.fs", "let ``unfinished", "`` = 2\n"),
        ] {
            let source = original_byte_limited("module Example\nlet prior = 1\n", tail);
            cases.push((path, format!("{source}{remainder}"), true));
        }
        for little_endian in [true, false] {
            let temporary = local_tempdir();
            for (path, source, _) in &cases {
                write_fixture(&temporary, path, &encode(source, little_endian));
            }
            let config = ConfigSnapshot::resolve(&[]).unwrap();
            let root = fixture_root(&temporary, b"fsharp-decoded-prefix");
            let policy = DiscoveryPolicy::build(Vec::new(), true).unwrap();
            let result = discover_with_snapshots(
                &root,
                &config,
                &policy,
                DiscoveryLimits::from_config(&config),
                BTreeMap::new(),
                &Cancellation::new(),
            )
            .unwrap();
            assert_eq!(result.manifest.inputs.len(), cases.len());
            assert!(result.manifest.exclusions.is_empty());
            for (path, source, expected_hint) in &cases {
                let input = result
                    .manifest
                    .inputs
                    .iter()
                    .find(|input| input.path == *path)
                    .unwrap();
                let bytes = encode(source, little_endian);
                assert_eq!(
                    input.encoding,
                    if little_endian {
                        "utf-16le"
                    } else {
                        "utf-16be"
                    }
                );
                assert_eq!(input.content_hash, content_hash(&bytes));
                assert_eq!(input.bytes, u64::try_from(bytes.len()).unwrap());
                assert_eq!(result.snapshots.get(&input.file).unwrap().content(), bytes);
                let expected = if *expected_hint {
                    vec![LanguageSignal {
                        language: "fsharp".to_owned(),
                        evidence: LanguageEvidence::Content,
                    }]
                } else {
                    Vec::new()
                };
                assert_eq!(
                    input.language_signals, expected,
                    "{path}; little endian={little_endian}"
                );
            }
        }
    }

    #[test]
    fn fsharp_return_annotations_survive_decoded_sampling() {
        let temporary = local_tempdir();
        let mut cases = Vec::new();
        for (newline_name, newline) in [("Lf", "\n"), ("CrLf", "\r\n")] {
            let prefix = "module Example\nlet prior = 1\nlet build<'Item when 'Item : comparison>\n    (value: 'Item[])\n    (items: Example.Item option)\n    (pairs: (int * string) list)\n    : Example.Result<'Item> list =\n        []\n"
                .replace('\n', newline);
            let complete = prefix.clone();
            let sampled = format!("{prefix}//{}", " ".repeat(MAX_CLASSIFICATION_BYTES));
            let mut partial_body =
                "module Example\nlet prior = 1\nlet build\n    (value: int)\n    : int =\n"
                    .replace('\n', newline);
            partial_body.push_str(&" ".repeat(MAX_CLASSIFICATION_BYTES - partial_body.len() - 1));
            partial_body.push_str("value\n");
            for (case_name, source, recognized) in [
                ("Complete", complete, true),
                ("Sampled", sampled, true),
                ("PartialBody", partial_body, false),
            ] {
                for encoding in ["utf-8", "utf-16le", "utf-16be"] {
                    let bytes = if encoding == "utf-8" {
                        source.as_bytes().to_vec()
                    } else {
                        std::iter::once(0xfeff_u16)
                            .chain(source.encode_utf16())
                            .flat_map(|unit| {
                                if encoding == "utf-16le" {
                                    unit.to_le_bytes()
                                } else {
                                    unit.to_be_bytes()
                                }
                            })
                            .collect()
                    };
                    let path = format!("{newline_name}{case_name}{encoding}.fs");
                    write_fixture(&temporary, &path, &bytes);
                    let recognized = if case_name == "PartialBody" {
                        encoding == "utf-8"
                    } else {
                        recognized
                    };
                    cases.push((path, bytes, encoding, recognized));
                }
            }
        }
        let config = ConfigSnapshot::resolve(&[]).unwrap();
        let root = fixture_root(&temporary, b"fsharp-return-annotation");
        let policy = DiscoveryPolicy::build(Vec::new(), true).unwrap();
        let result = discover_with_snapshots(
            &root,
            &config,
            &policy,
            DiscoveryLimits::from_config(&config),
            BTreeMap::new(),
            &Cancellation::new(),
        )
        .unwrap();
        assert_eq!(result.manifest.inputs.len(), cases.len());
        assert!(result.manifest.exclusions.is_empty());
        for (path, bytes, encoding, recognized) in cases {
            let input = result
                .manifest
                .inputs
                .iter()
                .find(|input| input.path == path)
                .unwrap();
            assert_eq!(input.encoding, encoding, "{path}");
            assert_eq!(input.content_hash, content_hash(&bytes), "{path}");
            assert_eq!(input.bytes, u64::try_from(bytes.len()).unwrap(), "{path}");
            assert_eq!(result.snapshots.get(&input.file).unwrap().content(), bytes);
            let expected = if recognized {
                vec![LanguageSignal {
                    language: "fsharp".to_owned(),
                    evidence: LanguageEvidence::Content,
                }]
            } else {
                Vec::new()
            };
            assert_eq!(input.language_signals, expected, "{path}");
        }
    }

    #[test]
    fn fsharp_multiline_named_fields_preserve_decoded_shared_suffix_routes() {
        let temporary = local_tempdir();
        let mut cases = Vec::new();
        for (case, pattern, tail, sampled, recognized) in [
            (
                "Multiline",
                "(Packet(item=value\n        metadata=state))",
                "",
                true,
                true,
            ),
            (
                "MixedComplete",
                "(Packet(item=value\n        metadata=state ; code=other))",
                "",
                false,
                true,
            ),
            (
                "MixedSampled",
                "(Packet(item=value\n        metadata=state ; code=other))",
                "",
                true,
                true,
            ),
            (
                "ForthColon",
                "(Packet(item=value\n        metadata=state ; code=other))",
                ": word\n",
                false,
                false,
            ),
            (
                "ForthDefinition",
                "(Packet(item=value\n        metadata=state ; code=other))",
                ": word dup * ;\n",
                true,
                false,
            ),
            (
                "MissingValue",
                "(Packet(item=value\n        metadata=))",
                "",
                true,
                false,
            ),
            (
                "TupleOwnership",
                "(Packet(item=value)\n        metadata=state)",
                "",
                true,
                false,
            ),
        ] {
            for (newline_name, newline) in [("Lf", "\n"), ("CrLf", "\r\n")] {
                let mut source = format!("module Example\nlet prior = 1\nlet combine\n    {pattern}\n    : int =\n    1\n{tail}").replace('\n', newline);
                if sampled {
                    source.push_str("//");
                    source.push_str(&" ".repeat(MAX_CLASSIFICATION_BYTES));
                }
                for encoding in ["utf-8", "utf-16le", "utf-16be"] {
                    let bytes = if encoding == "utf-8" {
                        source.as_bytes().to_vec()
                    } else {
                        std::iter::once(0xfeff_u16)
                            .chain(source.encode_utf16())
                            .flat_map(|unit| {
                                if encoding == "utf-16le" {
                                    unit.to_le_bytes()
                                } else {
                                    unit.to_be_bytes()
                                }
                            })
                            .collect()
                    };
                    let path = format!("{case}{newline_name}{encoding}.fs");
                    write_fixture(&temporary, &path, &bytes);
                    cases.push((path, bytes, encoding, recognized));
                }
            }
        }
        let config = ConfigSnapshot::resolve(&[]).unwrap();
        let root = fixture_root(&temporary, b"fsharp-multiline-named-fields");
        let policy = DiscoveryPolicy::build(Vec::new(), true).unwrap();
        let result = discover_with_snapshots(
            &root,
            &config,
            &policy,
            DiscoveryLimits::from_config(&config),
            BTreeMap::new(),
            &Cancellation::new(),
        )
        .unwrap();
        assert_eq!(result.manifest.inputs.len(), cases.len());
        assert!(result.manifest.exclusions.is_empty());
        for (path, bytes, encoding, recognized) in cases {
            let input = result
                .manifest
                .inputs
                .iter()
                .find(|input| input.path == path)
                .unwrap();
            assert_eq!(input.encoding, encoding, "{path}");
            assert_eq!(input.content_hash, content_hash(&bytes), "{path}");
            assert_eq!(input.bytes, u64::try_from(bytes.len()).unwrap(), "{path}");
            assert_eq!(input.class, InputClass::Source, "{path}");
            assert!(input.project_language_witnesses.is_empty(), "{path}");
            assert_eq!(result.snapshots.get(&input.file).unwrap().content(), bytes);
            let expected = if recognized {
                vec![LanguageSignal {
                    language: "fsharp".to_owned(),
                    evidence: LanguageEvidence::Content,
                }]
            } else {
                Vec::new()
            };
            assert_eq!(input.language_signals, expected, "{path}");
        }
    }

    #[test]
    fn fsharp_decoded_sample_boundaries_preserve_only_prior_owned_declarations() {
        let temporary = local_tempdir();
        let mut cases = Vec::new();
        for (newline_name, newline) in [("Lf", "\n"), ("CrLf", "\r\n")] {
            for encoding in ["utf-8", "utf-16le", "utf-16be"] {
                let cutoff = if encoding == "utf-8" {
                    MAX_CLASSIFICATION_BYTES
                } else {
                    (MAX_CLASSIFICATION_BYTES - 2) / 2
                };
                for (case_name, prefix, head, body, recognized) in [
                    (
                        "Owned",
                        "module Example\nlet prior = 1\n",
                        "let build (value: int)",
                        "        value",
                        true,
                    ),
                    (
                        "NoAnchor",
                        "module Example\n",
                        "let build (value: int)",
                        "        value",
                        false,
                    ),
                    (
                        "InvalidOwner",
                        "module Example\nlet prior = 1\n",
                        "let build (: int)",
                        "        value",
                        false,
                    ),
                    (
                        "Conflict",
                        "module Example\nlet prior = 1\n: square dup * ;\n",
                        "let build (value: int)",
                        "        value",
                        false,
                    ),
                    (
                        "Offside",
                        "module Example\nlet prior = 1\n",
                        "let build (value: int)",
                        "value",
                        false,
                    ),
                ] {
                    let prefix = prefix.replace('\n', newline);
                    let head = format!("{head}{newline}    : int ={newline}");
                    let before_value = body.len() - "value".len();
                    let padding =
                        cutoff - prefix.len() - 2 - newline.len() - head.len() - before_value - 1;
                    let source = format!(
                        "{prefix}//{}{newline}{head}{body}{newline}",
                        " ".repeat(padding)
                    );
                    assert_eq!(source.as_bytes()[cutoff - 1], b'v');
                    let bytes = if encoding == "utf-8" {
                        source.as_bytes().to_vec()
                    } else {
                        std::iter::once(0xfeff_u16)
                            .chain(source.encode_utf16())
                            .flat_map(|unit| {
                                if encoding == "utf-16le" {
                                    unit.to_le_bytes()
                                } else {
                                    unit.to_be_bytes()
                                }
                            })
                            .collect()
                    };
                    assert!(bytes.len() > MAX_CLASSIFICATION_BYTES);
                    let path = format!("{newline_name}{case_name}{encoding}.fs");
                    write_fixture(&temporary, &path, &bytes);
                    cases.push((path, bytes, encoding, recognized));
                }
            }
        }
        let root = fixture_root(&temporary, b"fsharp-decoded-owned-body-boundary");
        let config = ConfigSnapshot::resolve(&[]).unwrap();
        let policy = DiscoveryPolicy::build(Vec::new(), true).unwrap();
        let result = discover_with_snapshots(
            &root,
            &config,
            &policy,
            DiscoveryLimits::from_config(&config),
            BTreeMap::new(),
            &Cancellation::new(),
        )
        .unwrap();
        assert!(result.manifest.coverage.complete);
        assert!(result.manifest.exclusions.is_empty());
        assert_eq!(result.manifest.inputs.len(), cases.len());
        for (path, bytes, encoding, recognized) in cases {
            let input = result
                .manifest
                .inputs
                .iter()
                .find(|input| input.path == path)
                .unwrap();
            assert_eq!(input.encoding, encoding, "{path}");
            assert_eq!(input.content_hash, content_hash(&bytes), "{path}");
            assert_eq!(input.bytes, u64::try_from(bytes.len()).unwrap(), "{path}");
            assert_eq!(result.snapshots.get(&input.file).unwrap().content(), bytes);
            let expected = if recognized {
                vec![LanguageSignal {
                    language: "fsharp".to_owned(),
                    evidence: LanguageEvidence::Content,
                }]
            } else {
                Vec::new()
            };
            assert_eq!(input.language_signals, expected, "{path}");
        }
    }

    #[test]
    fn fsharp_finite_prologues_require_decoded_complete_anchors() {
        let temporary = local_tempdir();
        let mut cases = Vec::new();
        for (case_name, prologue, recognized) in [
            ("Open", "open Example.Support\n", true),
            ("Attribute", "[<AutoOpen>]\n", true),
            ("Warning", "#nowarn \"9\"\n", true),
            (
                "WarningComment",
                "#nowarn \"9\" // scoped warning policy\n",
                true,
            ),
            (
                "Combined",
                "#nowarn \"9\"\nopen Example.Support\n[<AutoOpen>]\n",
                true,
            ),
            ("Conditional", "#if FEATURE\n", false),
            ("OpenType", "open type Example.Support\n", false),
            ("MalformedPath", "open Example..Support\n", false),
            ("AttributeArgument", "[<Example.Attribute(1)>]\n", false),
            ("NonNumericWarning", "#nowarn \"other\"\n", false),
        ] {
            for (newline_name, newline) in [("Lf", "\n"), ("CrLf", "\r\n")] {
                let source = format!(
                    "{prologue}module Example =\n    let value = 1\n//{}",
                    " ".repeat(MAX_CLASSIFICATION_BYTES),
                )
                .replace('\n', newline);
                for encoding in ["utf-8", "utf-16le", "utf-16be"] {
                    let bytes = if encoding == "utf-8" {
                        source.as_bytes().to_vec()
                    } else {
                        std::iter::once(0xfeff_u16)
                            .chain(source.encode_utf16())
                            .flat_map(|unit| {
                                if encoding == "utf-16le" {
                                    unit.to_le_bytes()
                                } else {
                                    unit.to_be_bytes()
                                }
                            })
                            .collect()
                    };
                    let path = format!("{case_name}{newline_name}{encoding}.fs");
                    write_fixture(&temporary, &path, &bytes);
                    cases.push((path, bytes, encoding, recognized));
                }
            }
        }
        let config = ConfigSnapshot::resolve(&[]).unwrap();
        let root = fixture_root(&temporary, b"fsharp-finite-prologue");
        let policy = DiscoveryPolicy::build(Vec::new(), true).unwrap();
        let result = discover_with_snapshots(
            &root,
            &config,
            &policy,
            DiscoveryLimits::from_config(&config),
            BTreeMap::new(),
            &Cancellation::new(),
        )
        .unwrap();
        assert_eq!(result.manifest.inputs.len(), cases.len());
        assert!(result.manifest.exclusions.is_empty());
        for (path, bytes, encoding, recognized) in cases {
            let input = result
                .manifest
                .inputs
                .iter()
                .find(|input| input.path == path)
                .unwrap();
            assert_eq!(input.encoding, encoding, "{path}");
            assert_eq!(input.content_hash, content_hash(&bytes), "{path}");
            assert_eq!(input.bytes, u64::try_from(bytes.len()).unwrap(), "{path}");
            assert_eq!(result.snapshots.get(&input.file).unwrap().content(), bytes);
            let expected = if recognized {
                vec![LanguageSignal {
                    language: "fsharp".to_owned(),
                    evidence: LanguageEvidence::Content,
                }]
            } else {
                Vec::new()
            };
            assert_eq!(input.language_signals, expected, "{path}");
        }
    }

    #[test]
    fn fsharp_conditional_scopes_require_complete_decoded_alternatives() {
        let temporary = local_tempdir();
        let mut cases = Vec::new();
        for (case, source, recognized) in [
            (
                "Namespaces",
                "#if FEATURE\nnamespace Example.Enabled\n#else\nnamespace Example.Disabled\n#endif\nlet value = 1\n",
                true,
            ),
            (
                "Modules",
                "#if FEATURE\nmodule internal Example.Enabled\n#else\nmodule Example.Disabled\n#endif\nlet value = 1\n",
                true,
            ),
            (
                "MalformedArm",
                "#if FEATURE\nnamespace Example.Enabled\n#else\nnamespace Example.\n#endif\nlet value = 1\n",
                false,
            ),
            (
                "Unclosed",
                "#if FEATURE\nnamespace Example.Enabled\n#else\nnamespace Example.Disabled\nlet value = 1\n",
                false,
            ),
            (
                "Mixed",
                "#if FEATURE\nnamespace Example.Enabled\n#else\nnamespace Example.Disabled\n#endif\nlet value = 1\n: square dup * ;\n",
                false,
            ),
        ] {
            for (newline_name, newline) in [("Lf", "\n"), ("CrLf", "\r\n")] {
                let source = format!("{source}//{}", " ".repeat(MAX_CLASSIFICATION_BYTES))
                    .replace('\n', newline);
                for encoding in ["utf-8", "utf-16le", "utf-16be"] {
                    let bytes = if encoding == "utf-8" {
                        source.as_bytes().to_vec()
                    } else {
                        std::iter::once(0xfeff_u16)
                            .chain(source.encode_utf16())
                            .flat_map(|unit| {
                                if encoding == "utf-16le" {
                                    unit.to_le_bytes()
                                } else {
                                    unit.to_be_bytes()
                                }
                            })
                            .collect()
                    };
                    let path = format!("{case}{newline_name}{encoding}.fs");
                    write_fixture(&temporary, &path, &bytes);
                    cases.push((path, bytes, encoding, recognized));
                }
            }
        }
        let config = ConfigSnapshot::resolve(&[]).unwrap();
        let root = fixture_root(&temporary, b"fsharp-conditional-scopes");
        let policy = DiscoveryPolicy::build(Vec::new(), true).unwrap();
        let result = discover_with_snapshots(
            &root,
            &config,
            &policy,
            DiscoveryLimits::from_config(&config),
            BTreeMap::new(),
            &Cancellation::new(),
        )
        .unwrap();
        assert_eq!(result.manifest.inputs.len(), cases.len());
        assert!(result.manifest.exclusions.is_empty());
        for (path, bytes, encoding, recognized) in cases {
            let input = result
                .manifest
                .inputs
                .iter()
                .find(|input| input.path == path)
                .unwrap();
            assert_eq!(input.encoding, encoding, "{path}");
            assert_eq!(input.content_hash, content_hash(&bytes), "{path}");
            assert_eq!(input.bytes, u64::try_from(bytes.len()).unwrap(), "{path}");
            assert!(input.project_language_witnesses.is_empty(), "{path}");
            assert_eq!(result.snapshots.get(&input.file).unwrap().content(), bytes);
            let expected = if recognized {
                vec![LanguageSignal {
                    language: "fsharp".to_owned(),
                    evidence: LanguageEvidence::Content,
                }]
            } else {
                Vec::new()
            };
            assert_eq!(input.language_signals, expected, "{path}");
        }
    }

    #[test]
    fn fsharp_shared_suffix_keeps_complete_evidence_before_sample_truncation() {
        let path = RelativePath::parse(Path::new("Library.fs")).unwrap();
        for (tail, remainder) in [
            ("let next = \"unfinished", "\"\n"),
            ("(* unfinished", " *)\n"),
            ("\u{03bb}", "\n"),
        ] {
            let mut source = b"module Example\nlet value = 1\n".to_vec();
            let sampled_tail_bytes = if tail == "\u{03bb}" { 1 } else { tail.len() };
            source.resize(MAX_CLASSIFICATION_BYTES - sampled_tail_bytes, b' ');
            source.extend_from_slice(tail.as_bytes());
            source.extend_from_slice(remainder.as_bytes());
            assert_eq!(
                classify(&path, &source).1,
                vec![LanguageSignal {
                    language: "fsharp".to_owned(),
                    evidence: LanguageEvidence::Content,
                }],
                "{tail:?}"
            );
            let other = RelativePath::parse(Path::new("Library.txt")).unwrap();
            assert!(classify(&other, &source).1.is_empty());
        }
    }

    #[test]
    fn fsharp_routes_keep_implementation_and_signature_profiles_distinct() {
        for (language, suffixes) in [
            ("fsharp", &[".fsscript", ".fsx"][..]),
            ("fsharp-signature", &[".fsi"][..]),
        ] {
            let capability = language_capabilities()
                .iter()
                .find(|entry| entry.language == language)
                .unwrap();
            assert_eq!(capability.suffixes, suffixes);
            let detectors: &[&str] = if language == "fsharp" {
                &["content", "extension", "configuration"]
            } else {
                &["extension", "configuration"]
            };
            assert_eq!(capability.detectors, detectors);
            assert_eq!(capability.analyzers, ["treesitter"]);
            assert_eq!(capability.maximum_tier, "tier_d");
            assert_eq!(canonical_language(language), Some(language));
            for suffix in suffixes {
                for path in [
                    format!("Sample{suffix}"),
                    format!("src/SAMPLE{suffix}").to_uppercase(),
                ] {
                    assert_eq!(extension_language(&path), Some(language));
                }
            }
        }
        for path in [
            "sample.fs",
            "sample.FS",
            "sample.fsx.bak",
            "sample.fsscript.bak",
            "sample.fsi.txt",
        ] {
            assert_eq!(extension_language(path), None, "{path}");
        }
    }

    #[test]
    fn fsscript_routes_do_not_depend_on_declaration_shape() {
        let source = b"printfn \"ready\"\r\n";
        for path in ["Script.fsscript", "src/SCRIPT.FSSCRIPT", "Script.fsx"] {
            let path = RelativePath::parse(Path::new(path)).unwrap();
            assert_eq!(
                classify(&path, source),
                (
                    InputClass::Source,
                    vec![LanguageSignal {
                        language: "fsharp".to_owned(),
                        evidence: LanguageEvidence::Extension,
                    }],
                )
            );
        }
        let ambiguous = RelativePath::parse(Path::new("Script.fs")).unwrap();
        assert!(classify(&ambiguous, source).1.is_empty());
        let signature = RelativePath::parse(Path::new("Contract.fsi")).unwrap();
        assert_eq!(
            classify(&signature, b"val value: int\n").1,
            vec![LanguageSignal {
                language: "fsharp-signature".to_owned(),
                evidence: LanguageEvidence::Extension,
            }]
        );
    }

    #[test]
    fn configured_fsscript_preserves_original_sources_and_policy_barriers() {
        let temporary = local_tempdir();
        let script = "\u{feff}// Unicode source preservation: λ\r\nprintfn \"ready\"\r\n";
        let signature = b"module Contract\r\nval value: int\r\n";
        for path in [
            "Script.fsscript",
            "Script.fsx",
            "Ambiguous.fs",
            "denied.fsscript",
        ] {
            write_fixture(&temporary, path, script.as_bytes());
        }
        write_fixture(&temporary, "Configured.fsscript", signature);
        write_fixture(&temporary, "Contract.fsi", signature);
        write_fixture(&temporary, "binary.fsscript", b"\0binary");
        let config = ConfigSnapshot::resolve(&[rootlight_config::ConfigLayer {
            source: rootlight_config::ConfigSource::User,
            contents: "version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.extensions]\nfsscript='fsharp'\n[extensions.source-languages.data.paths]\n'Configured.fsscript'='fsharp-signature'\n'denied.fsscript'='fsharp'\n'binary.fsscript'='fsharp'\n",
        }])
        .unwrap();
        assert_eq!(
            config.source_language_for_path("Configured.fsscript"),
            Some("fsharp-signature")
        );
        let root = fixture_root(&temporary, b"configured-fsscript");
        let policy = DiscoveryPolicy::build(
            vec![PolicyRule {
                layer: PolicyLayer::Operation,
                pattern: "denied.fsscript".to_owned(),
                source: "fixture".to_owned(),
            }],
            true,
        )
        .unwrap();
        let (manifest, snapshots) = discover_with_snapshots(
            &root,
            &config,
            &policy,
            limits(),
            BTreeMap::new(),
            &Cancellation::new(),
        )
        .unwrap()
        .into_parts();
        assert_eq!(manifest.configuration_hash, config.hash());
        assert_eq!(manifest.inputs.len(), 5);
        assert_eq!(snapshots.len(), 5);
        for (path, content) in [
            ("Script.fsscript", script.as_bytes()),
            ("Script.fsx", script.as_bytes()),
            ("Ambiguous.fs", script.as_bytes()),
            ("Configured.fsscript", signature.as_slice()),
            ("Contract.fsi", signature.as_slice()),
        ] {
            let input = manifest
                .inputs
                .iter()
                .find(|input| input.path == path)
                .unwrap();
            let path = RelativePath::parse(Path::new(path)).unwrap();
            assert_eq!(input.file, root.file_id(&path));
            assert_eq!(input.class, InputClass::Source);
            assert_eq!(input.encoding, "utf-8");
            assert_eq!(input.bytes, u64::try_from(content.len()).unwrap());
            assert_eq!(input.content_hash, content_hash(content));
            assert_eq!(snapshots[&input.file].content(), content);
            assert!(input.project_language_witnesses.is_empty());
            let expected = match input.path.as_str() {
                "Script.fsscript" => vec![
                    LanguageSignal {
                        language: "fsharp".to_owned(),
                        evidence: LanguageEvidence::Extension,
                    },
                    LanguageSignal {
                        language: "fsharp".to_owned(),
                        evidence: LanguageEvidence::Configuration,
                    },
                ],
                "Configured.fsscript" => vec![
                    LanguageSignal {
                        language: "fsharp".to_owned(),
                        evidence: LanguageEvidence::Extension,
                    },
                    LanguageSignal {
                        language: "fsharp-signature".to_owned(),
                        evidence: LanguageEvidence::Configuration,
                    },
                ],
                "Script.fsx" => vec![LanguageSignal {
                    language: "fsharp".to_owned(),
                    evidence: LanguageEvidence::Extension,
                }],
                "Contract.fsi" => vec![LanguageSignal {
                    language: "fsharp-signature".to_owned(),
                    evidence: LanguageEvidence::Extension,
                }],
                _ => Vec::new(),
            };
            assert_eq!(input.language_signals, expected, "{}", input.path);
        }
        for (path, reason) in [
            ("denied.fsscript", ExclusionReason::Policy),
            ("binary.fsscript", ExclusionReason::Binary),
        ] {
            assert!(
                manifest
                    .exclusions
                    .iter()
                    .any(|item| item.path == path && item.reason == reason)
            );
            assert!(!manifest.inputs.iter().any(|input| input.path == path));
        }
    }

    #[test]
    fn configured_fsharp_preserves_source_bytes_and_binary_exclusions() {
        for newline in ["\n", "\r\n"] {
            let temporary = local_tempdir();
            let implementation = "module Sample\nlet value = 1\n".replace('\n', newline);
            let signature = "module Sample\nval value: int\n".replace('\n', newline);
            for (path, source) in [
                ("Library.fs", implementation.as_str()),
                ("Script.fsx", implementation.as_str()),
                ("Contract.fs", signature.as_str()),
                ("Contract.fsi", signature.as_str()),
            ] {
                write_fixture(&temporary, path, source.as_bytes());
            }
            write_fixture(&temporary, "binary.fs", b"\0binary");
            let config = ConfigSnapshot::resolve(&[rootlight_config::ConfigLayer {
                source: rootlight_config::ConfigSource::User,
                contents: "version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.extensions]\nfs='fsharp'\n[extensions.source-languages.data.paths]\n'Contract.fs'='fsharp-signature'\n",
            }])
            .unwrap();
            let root = fixture_root(&temporary, b"configured-fsharp");
            let (manifest, snapshots) = discover_with_snapshots_at_limit(
                &root,
                &config,
                &DiscoveryPolicy::build(Vec::new(), true).unwrap(),
                DiscoveryLimits::from_config(&config),
                BTreeMap::new(),
                4096,
                &Cancellation::new(),
            )
            .unwrap()
            .into_parts();
            assert_eq!(manifest.configuration_hash, config.hash());
            assert_eq!(manifest.inputs.len(), 4);
            assert_eq!(snapshots.len(), 4);
            assert_eq!(manifest.exclusions.len(), 1);
            assert_eq!(manifest.exclusions[0].path, "binary.fs");
            assert_eq!(manifest.exclusions[0].reason, ExclusionReason::Binary);
            for (path, language, content, evidence) in [
                (
                    "Library.fs",
                    "fsharp",
                    implementation.as_bytes(),
                    LanguageEvidence::Configuration,
                ),
                (
                    "Script.fsx",
                    "fsharp",
                    implementation.as_bytes(),
                    LanguageEvidence::Extension,
                ),
                (
                    "Contract.fs",
                    "fsharp-signature",
                    signature.as_bytes(),
                    LanguageEvidence::Configuration,
                ),
                (
                    "Contract.fsi",
                    "fsharp-signature",
                    signature.as_bytes(),
                    LanguageEvidence::Extension,
                ),
            ] {
                let input = manifest
                    .inputs
                    .iter()
                    .find(|input| input.path == path)
                    .unwrap();
                assert_eq!(input.class, InputClass::Source);
                assert_eq!(input.encoding, "utf-8");
                assert_eq!(input.bytes, u64::try_from(content.len()).unwrap());
                assert_eq!(input.content_hash, content_hash(content));
                assert_eq!(snapshots[&input.file].content(), content);
                assert!(
                    input
                        .language_signals
                        .iter()
                        .any(|signal| signal.language == language && signal.evidence == evidence)
                );
            }
        }
    }

    #[test]
    fn haskell_discovery_retains_unknown_text_and_excludes_binary_content() {
        let temporary = local_tempdir();
        let sources = [
            (
                "src/Main.hs",
                b"module Example where\nvalue = 1\n".as_slice(),
                Some("haskell"),
            ),
            (
                "docs/Main.lhs",
                b"> module Example where\n> value = 1\n".as_slice(),
                None,
            ),
            (
                "src/Foreign.hsc",
                b"module Example where\nvalue = #{const 1}\n".as_slice(),
                None,
            ),
        ];
        for &(path, content, _) in &sources {
            write_fixture(&temporary, path, content);
        }
        write_fixture(&temporary, "src/binary.hs", b"\0opaque payload");
        let root = fixture_root(&temporary, b"haskell-input-accounting");
        let policy = DiscoveryPolicy::build(Vec::new(), true).unwrap();
        let (manifest, snapshots) = discover_with_snapshots_at_limit(
            &root,
            &config(),
            &policy,
            limits(),
            BTreeMap::new(),
            4096,
            &Cancellation::new(),
        )
        .unwrap()
        .into_parts();
        assert_eq!(manifest.inputs.len(), sources.len());
        assert_eq!(snapshots.len(), sources.len());
        assert_eq!(manifest.coverage.included, 3);
        assert_eq!(manifest.exclusions.len(), 1);
        assert_eq!(manifest.exclusions[0].path, "src/binary.hs");
        assert_eq!(manifest.exclusions[0].reason, ExclusionReason::Binary);
        for (path, content, language) in sources {
            let input = manifest
                .inputs
                .iter()
                .find(|input| input.path == path)
                .unwrap();
            assert_eq!(input.class, InputClass::Source);
            assert_eq!(input.encoding, "utf-8");
            assert_eq!(input.bytes, u64::try_from(content.len()).unwrap());
            assert_eq!(input.content_hash, content_hash(content));
            assert_eq!(snapshots[&input.file].content(), content);
            assert_eq!(
                input.language_signals,
                language
                    .into_iter()
                    .map(|language| LanguageSignal {
                        language: language.to_owned(),
                        evidence: LanguageEvidence::Extension,
                    })
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn tcl_capability_classifies_scripts_without_claiming_backups() {
        let capability = language_capabilities()
            .iter()
            .find(|entry| entry.language == "tcl")
            .expect("Tcl capability is declared");
        assert_eq!(capability.analyzers, ["native"]);
        assert_eq!(capability.maximum_tier, "tier_d");
        for path in ["script.tcl", "widget.tk", "SCRIPT.TCL"] {
            assert_eq!(extension_language(path), Some("tcl"), "{path}");
        }
        assert_eq!(extension_language("script.tcl.bak"), None);
        assert_eq!(canonical_language("tk"), Some("tcl"));
        let (class, signals) = classify(
            &RelativePath::parse(Path::new("runner")).expect("fixture path is valid"),
            b"#!/usr/bin/env tclsh\nproc main {} {}\n",
        );
        assert_eq!(class, InputClass::Source);
        assert!(signals.iter().any(|signal| {
            signal.language == "tcl" && signal.evidence == LanguageEvidence::Shebang
        }));
    }

    #[test]
    fn groovy_initializers_keep_only_their_extension_evidence() {
        let path = RelativePath::parse(Path::new("script.groovy")).unwrap();
        let (class, signals) = classify(&path, b"def value = factory(option: false)\n");
        assert_eq!(class, InputClass::Source);
        assert_eq!(
            signals,
            vec![LanguageSignal {
                language: "groovy".to_owned(),
                evidence: LanguageEvidence::Extension,
            }]
        );
    }

    #[test]
    fn python_content_hints_respect_the_existing_byte_prefix_bound() {
        let mut source = vec![b' '; MAX_CLASSIFICATION_BYTES];
        source.extend_from_slice(b"def read(value): return value\n");
        assert_eq!(content_language(&source), None);
        for byte in 0..=u8::MAX {
            let mut source = b"def ".to_vec();
            source.resize(MAX_CLASSIFICATION_BYTES - 1, byte);
            source.extend_from_slice(b"\xf0\x9f\x98\x80(value): return value\n");
            assert_eq!(content_language(&source), None);
        }
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
    fn unparsed_language_suffixes_advertise_only_file_retrieval() {
        for (path, language) in [
            ("Shape.impl.carbon", "carbon"),
            ("Main.idr", "idris"),
            ("Main.lidr", "idris"),
            ("Algebra.lean", "lean"),
            ("actor.pony", "pony"),
            ("theme.less", "less"),
            ("theme.sass", "sass"),
        ] {
            assert_eq!(extension_language(path), Some(language), "{path}");
            let (_, signals) = classify(
                &RelativePath::parse(Path::new(path)).expect("fixture path is valid"),
                b"source content\n",
            );
            assert!(signals.iter().any(|signal| {
                signal.language == language && signal.evidence == LanguageEvidence::Extension
            }));
            assert_eq!(canonical_language(language), Some(language));
            let capability = language_capabilities()
                .iter()
                .find(|capability| capability.language == language)
                .expect("source capability is declared");
            assert_eq!(capability.analyzers, &["source-fallback"]);
            assert_eq!(capability.maximum_tier, "tier_d");
        }
        for path in [
            "Main.agdai",
            "Main.olean",
            "Main.elm.bak",
            "actor.pony.txt",
            "theme.scss.bak",
            "theme.sass.txt",
            "theme.less.map",
        ] {
            assert_eq!(extension_language(path), None, "{path}");
        }
    }

    #[test]
    fn agda_source_and_literate_suffixes_advertise_native_structure() {
        let capability = language_capabilities()
            .iter()
            .find(|capability| capability.language == "agda")
            .expect("Agda capability is declared");
        assert_eq!(capability.analyzers, &["agda-native-structural-1"]);
        assert_eq!(capability.maximum_tier, "tier_d");
        for path in [
            "Module.agda",
            "Guide.lagda",
            "Guide.lagda.md",
            "Guide.lagda.rst",
            "Guide.lagda.tex",
        ] {
            assert_eq!(extension_language(path), Some("agda"), "{path}");
        }
    }

    #[test]
    fn first_party_parsers_advertise_native_structural_analysis() {
        for language in ["awk", "elm", "ini", "make", "prolog", "scss"] {
            let capability = language_capabilities()
                .iter()
                .find(|capability| capability.language == language)
                .expect("native language capability exists");
            assert_eq!(capability.analyzers, ["native"]);
            assert_eq!(capability.maximum_tier, "tier_d");
        }
    }

    #[test]
    fn ini_extension_keeps_source_and_backup_files_distinct() {
        assert_eq!(extension_language("config/settings.ini"), Some("ini"));
        assert_eq!(extension_language("config/SETTINGS.INI"), Some("ini"));
        assert_ne!(extension_language("config/settings.ini.bak"), Some("ini"));
        assert_eq!(canonical_language("ini"), Some("ini"));
    }

    #[test]
    fn awk_extension_keeps_source_and_backup_files_distinct() {
        assert_eq!(extension_language("scripts/report.awk"), Some("awk"));
        assert_eq!(extension_language("scripts/REPORT.AWK"), Some("awk"));
        assert_ne!(extension_language("scripts/report.awk.bak"), Some("awk"));
        assert_eq!(canonical_language("gawk"), Some("awk"));
    }

    #[test]
    fn apex_class_content_does_not_take_over_basic_suffixes() {
        for path in ["src/Box.cls", "src/BOX.CLS"] {
            let path = RelativePath::parse(Path::new(path)).unwrap();
            let (_, signals) = classify(&path, b"@IsTest public class Box {}");
            assert!(signals.iter().any(|signal| {
                signal.language == "apex" && signal.evidence == LanguageEvidence::Content
            }));
        }
        for path in ["src/Box.bas", "src/Box.frm", "src/Box.cls.bak"] {
            let path = RelativePath::parse(Path::new(path)).unwrap();
            let (_, signals) = classify(&path, b"public class Box {}");
            assert!(!signals.iter().any(|signal| signal.language == "apex"));
        }
        let path = RelativePath::parse(Path::new("src/Box.cls")).unwrap();
        for source in [
            "VERSION 1.0 CLASS\r\nAttribute VB_Name = \"Box\"\r\n",
            "Attribute VB_Name = \"Box\"\nvalue = \"public class Fake {}\"\n",
            "Attribute VB_Name = \"Box\"\nRem documentation; public class Fake {}\n",
            "Attribute VB_Name = \"Box\"\nvalue = 1: Rem documentation; class Fake {}\n",
            "// public class Fake {}",
            "public class Incomplete",
        ] {
            let (_, signals) = classify(&path, source.as_bytes());
            assert!(
                !signals.iter().any(|signal| signal.language == "apex"),
                "{source}"
            );
        }
        let (_, signals) = classify(
            &path,
            b"Attribute VB_Name = \"Box\"\r\npublic class Other {}\r\n",
        );
        for language in ["apex", "visual-basic-classic"] {
            assert!(signals.iter().any(|signal| {
                signal.language == language && signal.evidence == LanguageEvidence::Content
            }));
        }
        let capability = language_capabilities()
            .iter()
            .find(|capability| capability.language == "apex")
            .unwrap();
        assert!(capability.suffixes.is_empty());
        assert_eq!(capability.detectors, &["content", "configuration"]);
        assert_eq!(capability.analyzers, &["treesitter"]);
    }

    #[test]
    fn apex_class_hints_require_a_complete_bounded_original_header() {
        let path = RelativePath::parse(Path::new("src/Box.cls")).unwrap();
        let prefix = "public class Box extends ";
        let truncated = format!("{prefix}{}Base {{}}", " ".repeat(MAX_CLASSIFICATION_BYTES));
        let (_, signals) = classify(&path, truncated.as_bytes());
        assert!(!signals.iter().any(|signal| signal.language == "apex"));
        for little_endian in [true, false] {
            let source = "\u{feff}@IsTest\r\nprivate class Box {}";
            let encoded = source
                .encode_utf16()
                .flat_map(|unit| {
                    if little_endian {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    }
                })
                .collect::<Vec<_>>();
            let classification = encoding::classify(&encoded, &Cancellation::new())
                .unwrap()
                .unwrap();
            assert_eq!(
                classification.encoding,
                if little_endian {
                    "utf-16le"
                } else {
                    "utf-16be"
                }
            );
            let projected = classification.decoded_prefix.as_deref().unwrap();
            let (_, signals) = classify_prefix(
                &path,
                projected.as_bytes(),
                classification.prefix_truncated,
                true,
            );
            assert!(signals.iter().any(|signal| {
                signal.language == "apex" && signal.evidence == LanguageEvidence::Content
            }));
        }
    }

    #[test]
    fn apex_complete_headers_survive_unrelated_split_utf8_sample_tails() {
        let path = RelativePath::parse(Path::new("src/Written.cls")).unwrap();
        let mut source = b"public class Written {}\n// ".to_vec();
        source.resize(MAX_CLASSIFICATION_BYTES - 1, b' ');
        source.extend_from_slice("é".as_bytes());
        assert!(std::str::from_utf8(&source).is_ok());
        assert!(classify(&path, &source).1.iter().any(|signal| {
            signal.language == "apex" && signal.evidence == LanguageEvidence::Content
        }));
        source[MAX_CLASSIFICATION_BYTES - 2] = 0xff;
        assert!(
            !classify(&path, &source)
                .1
                .iter()
                .any(|signal| signal.language == "apex")
        );
    }

    #[test]
    fn sas_extension_preserves_distinct_sass_and_backup_routes() {
        for path in ["src/measure.sas", "src/MEASURE.SAS"] {
            assert_eq!(extension_language(path), Some("sas"));
        }
        assert_ne!(extension_language("src/measure.sas.bak"), Some("sas"));
        assert_eq!(extension_language("src/theme.sass"), Some("sass"));
        assert_eq!(canonical_language("sas"), Some("sas"));
        let capability = language_capabilities()
            .iter()
            .find(|capability| capability.language == "sas")
            .expect("SAS capability exists");
        assert_eq!(capability.suffixes, &[".sas"]);
        assert_eq!(capability.detectors, &["extension"]);
        assert_eq!(capability.maximum_tier, "tier_d");
        assert_eq!(capability.analyzers, &["source-fallback"]);
    }

    #[test]
    fn pascal_source_suffixes_advertise_only_dedicated_routes() {
        let suffixes = [".pas", ".dpr", ".lpr"];
        for suffix in suffixes {
            for path in [
                format!("src/Written{suffix}"),
                format!("src/WRITTEN{}", suffix.to_ascii_uppercase()),
            ] {
                assert_eq!(extension_language(&path), Some("pascal"), "{path}");
                assert_eq!(
                    classify(
                        &RelativePath::parse(Path::new(&path)).unwrap(),
                        b"begin\nend.\n",
                    )
                    .1,
                    vec![LanguageSignal {
                        language: "pascal".to_owned(),
                        evidence: LanguageEvidence::Extension,
                    }],
                    "{path}",
                );
            }
            assert_eq!(
                extension_language(&format!("src/Written{suffix}.bak")),
                None,
            );
        }
        for alias in ["pascal", "delphi", "object-pascal"] {
            assert_eq!(canonical_language(alias), Some("pascal"));
        }
        let capability = language_capabilities()
            .iter()
            .find(|capability| capability.language == "pascal")
            .expect("Pascal capability exists");
        assert_eq!(capability.suffixes, suffixes);
        assert_eq!(capability.aliases, ["delphi", "object-pascal"]);
        assert_eq!(capability.detectors, ["extension", "configuration"]);
        assert_eq!(capability.maximum_tier, "tier_d");
        assert_eq!(capability.analyzers, ["source-fallback"]);
    }

    #[test]
    fn pascal_shared_and_data_suffixes_do_not_claim_source_syntax() {
        for path in ["src/Written.pp", "src/WRITTEN.PP"] {
            assert_eq!(extension_language(path), None);
            for source in [
                "unit Written;\ninterface\nimplementation\nend.\n",
                "program Written;\nbegin\nend.\n",
                "class example { notify { 'ready': } }\n",
            ] {
                assert!(
                    classify(
                        &RelativePath::parse(Path::new(path)).unwrap(),
                        source.as_bytes()
                    )
                    .1
                    .iter()
                    .all(|signal| signal.language != "pascal"),
                    "{path}: {source}",
                );
            }
        }
        for (path, source) in [
            (
                "ui/Form.dfm",
                "object Form: TForm\n  Caption = 'Ready'\nend\n",
            ),
            ("ui/FORM.DFM", "object Form: TForm\nend\n"),
            ("Project.dproj", "<Project><PropertyGroup /></Project>\n"),
            ("PROJECT.DPROJ", "<Project />\n"),
        ] {
            assert_eq!(extension_language(path), None);
            assert!(
                classify(
                    &RelativePath::parse(Path::new(path)).unwrap(),
                    source.as_bytes()
                )
                .1
                .iter()
                .all(|signal| signal.language != "pascal"),
                "{path}",
            );
        }
    }

    #[test]
    fn configured_pascal_preserves_source_bytes_and_policy_barriers() {
        let temporary = local_tempdir();
        let signal = |language: &str, evidence| LanguageSignal {
            language: language.to_owned(),
            evidence,
        };
        let written = "\u{feff}unit Written;\r\ninterface\r\nconst Caption = '\u{03bb}';\r\nimplementation\r\nend.\r\n";
        let mut cases = vec![
            (
                "Module.pas",
                written.as_bytes().to_vec(),
                "utf-8",
                vec![signal("pascal", LanguageEvidence::Extension)],
            ),
            (
                "Program.dpr",
                b"program Written;\nbegin\nend.\n".to_vec(),
                "utf-8",
                vec![signal("pascal", LanguageEvidence::Extension)],
            ),
            (
                "Project.lpr",
                b"program Written;\r\nbegin\r\nend.\r\n".to_vec(),
                "utf-8",
                vec![signal("pascal", LanguageEvidence::Extension)],
            ),
            (
                "Configured.pp",
                written.as_bytes().to_vec(),
                "utf-8",
                vec![signal("pascal", LanguageEvidence::Configuration)],
            ),
            (
                "Default.pp",
                b"class example { notify { 'ready': } }\n".to_vec(),
                "utf-8",
                vec![signal("puppet", LanguageEvidence::Configuration)],
            ),
            (
                "Overridden.pas",
                written.as_bytes().to_vec(),
                "utf-8",
                vec![
                    signal("pascal", LanguageEvidence::Extension),
                    signal("unsupported-language", LanguageEvidence::Configuration),
                ],
            ),
            (
                "Form.dfm",
                b"object Form: TForm\nend\n".to_vec(),
                "utf-8",
                Vec::new(),
            ),
            (
                "Project.dproj",
                b"<Project />\n".to_vec(),
                "utf-8",
                Vec::new(),
            ),
        ];
        for (path, little_endian, encoding) in [
            ("Utf16LE.pas", true, "utf-16le"),
            ("Utf16BE.dpr", false, "utf-16be"),
        ] {
            let bytes = written
                .encode_utf16()
                .flat_map(|unit| {
                    if little_endian {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    }
                })
                .collect::<Vec<_>>();
            cases.push((
                path,
                bytes,
                encoding,
                vec![signal("pascal", LanguageEvidence::Extension)],
            ));
        }
        for (path, bytes, _, _) in &cases {
            write_fixture(&temporary, path, bytes);
        }
        write_fixture(&temporary, "denied.pas", written.as_bytes());
        write_fixture(&temporary, "binary.pas", b"\0binary");
        let config = ConfigSnapshot::resolve(&[rootlight_config::ConfigLayer {
            source: rootlight_config::ConfigSource::User,
            contents: "version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.extensions]\npp='puppet'\n[extensions.source-languages.data.paths]\n'Configured.pp'='pascal'\n'Overridden.pas'='unsupported-language'\n'denied.pas'='pascal'\n'binary.pas'='pascal'\n",
        }])
        .unwrap();
        assert_eq!(
            config.source_language_for_path("Configured.pp"),
            Some("pascal")
        );
        assert_eq!(
            config.source_language_for_path("Default.pp"),
            Some("puppet")
        );
        let root = fixture_root(&temporary, b"configured-pascal");
        let policy = DiscoveryPolicy::build(
            vec![PolicyRule {
                layer: PolicyLayer::Operation,
                pattern: "denied.pas".to_owned(),
                source: "fixture".to_owned(),
            }],
            true,
        )
        .unwrap();
        let (manifest, snapshots) = discover_with_snapshots(
            &root,
            &config,
            &policy,
            limits(),
            BTreeMap::new(),
            &Cancellation::new(),
        )
        .unwrap()
        .into_parts();
        assert_eq!(manifest.configuration_hash, config.hash());
        assert_eq!(manifest.inputs.len(), cases.len());
        assert_eq!(snapshots.len(), cases.len());
        assert!(manifest.coverage.complete);
        for (path, bytes, encoding, signals) in cases {
            let input = manifest
                .inputs
                .iter()
                .find(|input| input.path == path)
                .unwrap();
            let relative = RelativePath::parse(Path::new(path)).unwrap();
            assert_eq!(input.file, root.file_id(&relative), "{path}");
            assert_eq!(input.class, InputClass::Source, "{path}");
            assert_eq!(input.encoding, encoding, "{path}");
            assert_eq!(input.bytes, u64::try_from(bytes.len()).unwrap(), "{path}");
            assert_eq!(input.content_hash, content_hash(&bytes), "{path}");
            assert_eq!(snapshots[&input.file].content(), bytes, "{path}");
            assert_eq!(input.language_signals, signals, "{path}");
            assert!(input.project_language_witnesses.is_empty(), "{path}");
        }
        assert_eq!(manifest.exclusions.len(), 2);
        for (path, reason) in [
            ("denied.pas", ExclusionReason::Policy),
            ("binary.pas", ExclusionReason::Binary),
        ] {
            assert!(
                manifest
                    .exclusions
                    .iter()
                    .any(|entry| entry.path == path && entry.reason == reason),
                "{path}",
            );
            assert!(!manifest.inputs.iter().any(|input| input.path == path));
        }
    }

    #[test]
    fn plsql_dedicated_suffixes_do_not_replace_default_sql() {
        let suffixes = [".pks", ".pkb", ".plb", ".pls"];
        for suffix in suffixes {
            assert_eq!(
                extension_language(&format!("src/Package{suffix}")),
                Some("plsql")
            );
            assert_eq!(
                extension_language(&format!("src/PACKAGE{}", suffix.to_ascii_uppercase())),
                Some("plsql")
            );
            assert_ne!(
                extension_language(&format!("src/Package{suffix}.bak")),
                Some("plsql")
            );
        }
        assert_eq!(extension_language("src/schema.sql"), Some("sql"));
        assert_eq!(canonical_language("pl-sql"), Some("plsql"));
        let capability = language_capabilities()
            .iter()
            .find(|capability| capability.language == "plsql")
            .expect("PL/SQL capability exists");
        assert_eq!(capability.suffixes, suffixes);
        assert_eq!(capability.detectors, &["extension"]);
        assert_eq!(capability.maximum_tier, "tier_d");
        assert_eq!(capability.analyzers, &["treesitter"]);
    }

    #[test]
    fn fortran_free_form_suffixes_preserve_physical_template_routing() {
        let suffixes = [".f90", ".f95", ".f03", ".f08", ".fypp"];
        for suffix in suffixes {
            assert_eq!(
                extension_language(&format!("src/Sample{suffix}")),
                Some("fortran")
            );
            assert_eq!(
                extension_language(&format!("src/SAMPLE{}", suffix.to_ascii_uppercase())),
                Some("fortran")
            );
            assert_ne!(
                extension_language(&format!("src/Sample{suffix}.bak")),
                Some("fortran")
            );
        }
        for path in ["src/Sample.f", "src/Sample.for", "src/Sample.ftn"] {
            assert_ne!(extension_language(path), Some("fortran"));
        }
        let capability = language_capabilities()
            .iter()
            .find(|capability| capability.language == "fortran")
            .expect("Fortran capability exists");
        assert_eq!(capability.suffixes, suffixes);
        assert_eq!(capability.detectors, &["extension"]);
        assert_eq!(capability.maximum_tier, "tier_d");
        assert_eq!(capability.analyzers, &["treesitter"]);
    }

    #[test]
    fn erlang_source_and_header_extensions_have_native_capability() {
        for path in ["src/worker.erl", "include/worker.hrl", "src/worker.app.src"] {
            assert_eq!(extension_language(path), Some("erlang"));
        }
        assert_ne!(extension_language("src/worker.erl.bak"), Some("erlang"));
        let capability = language_capabilities()
            .iter()
            .find(|capability| capability.language == "erlang")
            .expect("Erlang capability exists");
        assert_eq!(capability.analyzers, &["native"]);
    }

    #[test]
    fn awk_shebang_recognizes_only_the_interpreter() {
        for header in [
            "#!/usr/bin/awk -f",
            "#!/usr/bin/gawk -f",
            "#!/usr/bin/env mawk",
            "#!/usr/bin/env -S nawk -f",
        ] {
            let source = format!("{header}\nBEGIN {{ print \"ready\" }}\n");
            assert_eq!(shebang_language(source.as_bytes()), Some("awk"), "{header}");
            let (_, signals) = classify(
                &RelativePath::parse(Path::new("scripts/report")).unwrap(),
                source.as_bytes(),
            );
            assert!(signals.iter().any(|signal| {
                signal.language == "awk" && signal.evidence == LanguageEvidence::Shebang
            }));
        }
        assert_eq!(shebang_language(b"#!/usr/bin/python awk\n"), Some("python"));
        assert_eq!(shebang_language(b"#!/usr/bin/env -S tool awk\n"), None);
    }

    #[test]
    fn make_source_names_have_native_filename_and_extension_evidence() {
        for path in ["Makefile", "GNUmakefile", "BSDmakefile", "build/MAKEFILE"] {
            assert_eq!(filename_language(path), Some("make"), "{path}");
        }
        for path in ["build.mk", "rules.MAK"] {
            assert_eq!(extension_language(path), Some("make"), "{path}");
        }
        for path in ["Makefile.bak", "Makefile.txt", "build.mk.bak"] {
            assert_ne!(filename_language(path), Some("make"), "{path}");
            assert_ne!(extension_language(path), Some("make"), "{path}");
        }
        assert_eq!(canonical_language("make"), Some("make"));
    }

    #[test]
    fn astro_detection_preserves_host_language_boundaries() {
        for path in ["view.astro", "pages/VIEW.ASTRO"] {
            assert_eq!(extension_language(path), Some("astro"), "{path}");
            let (_, signals) = classify(
                &RelativePath::parse(Path::new(path)).expect("fixture path is valid"),
                b"---\nconst title: string = 'Guide';\n---\n<main>{title}</main>",
            );
            assert!(signals.iter().any(|signal| {
                signal.language == "astro" && signal.evidence == LanguageEvidence::Extension
            }));
        }
        for path in [
            "view.astro.bak",
            "view.astro.ts",
            "view.mdx",
            "view.vue",
            "view.svelte",
        ] {
            assert_ne!(extension_language(path), Some("astro"), "{path}");
        }
        assert_eq!(canonical_language("astro"), Some("astro"));
        let capability = language_capabilities()
            .iter()
            .find(|capability| capability.language == "astro")
            .unwrap();
        assert_eq!(capability.analyzers, &["treesitter"]);
        assert_eq!(capability.maximum_tier, "tier_d");
    }

    #[test]
    fn nix_extensions_route_to_native_source_analysis_without_path_special_cases() {
        for path in [
            "default.nix",
            "flake.nix",
            "modules/service.nix",
            "MODULE.NIX",
        ] {
            assert_eq!(extension_language(path), Some("nix"));
        }
        assert_ne!(extension_language("module.nix.bak"), Some("nix"));
        assert_eq!(canonical_language("nix"), Some("nix"));
        let capability = language_capabilities()
            .iter()
            .find(|capability| capability.language == "nix")
            .unwrap();
        assert_eq!(capability.analyzers, &["treesitter"]);
        assert_eq!(capability.maximum_tier, "tier_d");
    }

    #[test]
    fn markdown_detection_preserves_host_language_boundaries() {
        for path in ["guide.md", "guide.markdown", "docs/GUIDE.MD"] {
            assert_eq!(extension_language(path), Some("markdown"), "{path}");
            let (_, signals) = classify(
                &RelativePath::parse(Path::new(path)).expect("fixture path is valid"),
                b"# Guide\n\n```rust\nfn example() {}\n```\n",
            );
            assert!(signals.iter().any(|signal| {
                signal.language == "markdown" && signal.evidence == LanguageEvidence::Extension
            }));
        }
        for path in [
            "view.mdx",
            "view.astro",
            "guide.md.bak",
            "guide.markdown.rs",
        ] {
            assert_ne!(extension_language(path), Some("markdown"), "{path}");
        }
        assert_eq!(canonical_language("markdown"), Some("markdown"));
        let capability = language_capabilities()
            .iter()
            .find(|capability| capability.language == "markdown")
            .expect("native document analysis is advertised");
        assert_eq!(capability.analyzers, &["treesitter"]);
        assert_eq!(capability.maximum_tier, "tier_d");
    }

    #[test]
    fn objc_native_capability_does_not_advertise_cpp_or_project_semantics() {
        for (language, analyzers) in [
            ("objective-c", &["treesitter"][..]),
            ("objective-cpp", &["source-fallback"][..]),
            ("matlab", &["treesitter"][..]),
        ] {
            let capability = language_capabilities()
                .iter()
                .find(|capability| capability.language == language)
                .expect("source capability is declared");
            assert_eq!(capability.analyzers, analyzers);
            assert_eq!(capability.maximum_tier, "tier_d");
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
            ("build.sbt", "scala"),
            ("script.sc", "scala"),
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
    fn assembly_capability_reports_the_native_structural_analyzer() {
        let capability = language_capabilities()
            .iter()
            .find(|entry| entry.language == "assembly")
            .expect("Assembly source capability");
        assert_eq!(capability.suffixes, [".asm", ".s"]);
        assert_eq!(capability.aliases, ["asm"]);
        assert_eq!(capability.detectors, ["extension"]);
        assert_eq!(capability.maximum_tier, "tier_d");
        assert_eq!(capability.analyzers, ["treesitter"]);
        assert_eq!(canonical_language("asm"), Some("assembly"));
        for path in ["source.asm", "source.s", "source.ASM", "source.S"] {
            assert_eq!(extension_language(path), Some("assembly"));
            let (class, signals) = classify(
                &RelativePath::parse(Path::new(path)).expect("fixture path"),
                b"Written PROC\nnop\nWritten ENDP\n",
            );
            assert_eq!(class, InputClass::Source);
            assert_eq!(
                signals,
                [LanguageSignal {
                    language: "assembly".to_owned(),
                    evidence: LanguageEvidence::Extension,
                }]
            );
        }
    }

    #[test]
    fn wgsl_admission_uses_only_its_exact_installed_suffix_and_canonical_name() {
        let capability = language_capabilities()
            .iter()
            .find(|entry| entry.language == "wgsl")
            .unwrap();
        assert_eq!(capability.suffixes, [".wgsl"]);
        assert!(capability.aliases.is_empty());
        assert_eq!(capability.detectors, ["extension"]);
        assert_eq!(capability.analyzers, ["treesitter"]);
        assert_eq!(capability.maximum_tier, "tier_d");
        assert_eq!(canonical_language("wgsl"), Some("wgsl"));
        for path in ["shader.wgsl", "SHADER.WGSL"] {
            assert_eq!(extension_language(path), Some("wgsl"));
            let (class, signals) = classify(
                &RelativePath::parse(Path::new(path)).unwrap(),
                b"fn Main() {}\n",
            );
            assert_eq!(class, InputClass::Source);
            assert_eq!(
                signals,
                [LanguageSignal {
                    language: "wgsl".to_owned(),
                    evidence: LanguageEvidence::Extension
                }]
            );
        }
        for path in [
            "shader.wgsl.bak",
            "shader.wgslx",
            "shader.wgsl/source",
            "source.wgs",
        ] {
            assert_ne!(extension_language(path), Some("wgsl"));
        }
        assert_eq!(extension_language("shader.glsl"), Some("glsl"));
        assert_eq!(extension_language("shader.hlsl"), Some("hlsl"));
        for language in ["shader", "source.wgsl", "hlsl", "glsl"] {
            assert_ne!(canonical_language(language), Some("wgsl"));
        }
    }

    #[test]
    fn shader_suffixes_retain_distinct_installed_capabilities() {
        for (language, suffixes, analyzers) in [
            (
                "glsl",
                &[".vert", ".frag", ".vsh", ".fsh", ".glsl"][..],
                &["treesitter"][..],
            ),
            (
                "hlsl",
                &[".hlsl", ".hlsli", ".fxh"][..],
                &["treesitter"][..],
            ),
        ] {
            let capability = language_capabilities()
                .iter()
                .find(|entry| entry.language == language)
                .expect("shader source capability");
            assert_eq!(capability.suffixes, suffixes);
            assert!(capability.aliases.is_empty());
            assert_eq!(capability.detectors, ["extension"]);
            assert_eq!(capability.maximum_tier, "tier_d");
            assert_eq!(capability.analyzers, analyzers);
            assert_eq!(canonical_language(language), Some(language));
            for suffix in suffixes {
                for path in [
                    format!("shaders/program{suffix}"),
                    format!("shaders/PROGRAM{}", suffix.to_ascii_uppercase()),
                ] {
                    assert_eq!(extension_language(&path), Some(language));
                    let (class, signals) = classify(
                        &RelativePath::parse(Path::new(&path)).expect("fixture path"),
                        b"void main() {}\n",
                    );
                    assert_eq!(class, InputClass::Source);
                    assert_eq!(
                        signals,
                        [LanguageSignal {
                            language: language.to_owned(),
                            evidence: LanguageEvidence::Extension,
                        }]
                    );
                }
                assert_eq!(extension_language(&format!("program{suffix}.bak")), None);
            }
        }
    }

    #[test]
    fn shader_routing_does_not_claim_shared_or_unaudited_suffixes() {
        for path in [
            "program.fs",
            "program.fx",
            "program.fxh.bak",
            "program.hlslx",
            "program.geom",
            "program.tesc",
            "program.tese",
            "program.comp",
            "program.glsl.map",
            "program.hlsl.txt",
            "program.hlsli.bak",
            "program.fxh.txt",
            "shaders.vert/program",
        ] {
            assert!(!matches!(extension_language(path), Some("glsl" | "hlsl")));
        }
        assert_eq!(extension_language("source.fsx"), Some("fsharp"));
        assert_eq!(extension_language("source.c"), Some("c"));
        assert_eq!(extension_language("source.cpp"), Some("cpp"));
        for alias in ["shader", "source.glsl", "source.cpp", "c", "cpp"] {
            assert!(!matches!(canonical_language(alias), Some("glsl" | "hlsl")));
        }
    }

    #[test]
    fn common_lisp_capability_reports_installed_written_source_provider() {
        let capability = language_capabilities()
            .iter()
            .find(|entry| entry.language == "common-lisp")
            .expect("Common Lisp capability");
        assert_eq!(capability.suffixes, [".lisp", ".asd"]);
        assert_eq!(capability.aliases, ["commonlisp", "common_lisp"]);
        assert_eq!(capability.detectors, ["content", "extension"]);
        assert_eq!(capability.maximum_tier, "tier_d");
        assert_eq!(capability.analyzers, ["treesitter"]);
        for alias in ["common-lisp", "commonlisp", "common_lisp"] {
            assert_eq!(canonical_language(alias), Some("common-lisp"));
        }
        for alias in ["lisp", "cl", "scheme", "emacs-lisp"] {
            assert_ne!(canonical_language(alias), Some("common-lisp"));
        }
        for path in ["src/Package.LISP", "system.asd"] {
            assert_eq!(extension_language(path), Some("common-lisp"));
        }
        for path in [
            "kernel.cl",
            "script.lsp",
            "library.scm",
            "library.ss",
            "library.sld",
            "editor.el",
            "package.lisp.bak",
        ] {
            assert_ne!(extension_language(path), Some("common-lisp"));
        }
    }

    #[test]
    fn common_lisp_content_requires_a_distinctive_written_opening_header() {
        for path in [
            "package.lisp",
            "system.asd",
            "ambiguous.cl",
            "ambiguous.lsp",
            "entry",
        ] {
            let path = RelativePath::parse(Path::new(path)).expect("fixture path");
            for source in [
                "(defpackage :sample (:use :cl))",
                "\u{feff}; header\r\n#| outer #| inner |# tail |# (IN-PACKAGE #:sample)",
                "(defclass widget (parent) ())",
                "(defgeneric measure (value))",
                "(defmethod measure :before ((value widget)) value)",
                "(defmethod (setf measure) (new-value (value widget)) new-value)",
                "(asdf:defsystem \"sample\" :components ())",
                "(ASDF::DEFSYSTEM #:sample)",
                "(cl:defun measure (value) value)",
                "(COMMON-LISP:DEFMACRO with-value (value) value)",
            ] {
                let (_, signals) = classify(&path, source.as_bytes());
                assert!(
                    signals.contains(&LanguageSignal {
                        language: "common-lisp".to_owned(),
                        evidence: LanguageEvidence::Content,
                    }),
                    "{}: {source}",
                    path.as_str(),
                );
            }
        }
    }

    #[test]
    fn common_lisp_content_does_not_admit_other_lisp_suffixes_or_generic_defun() {
        for name in [
            "library.scm",
            "library.ss",
            "library.sld",
            "editor.el",
            "backup.lisp.bak",
        ] {
            let path = RelativePath::parse(Path::new(name)).expect("fixture path");
            let (_, signals) = classify(&path, b"(defpackage :sample (:use :cl))");
            assert!(
                !signals
                    .iter()
                    .any(|signal| signal.language == "common-lisp"),
                "{name}"
            );
        }
        for name in ["kernel.cl", "script.lsp", "entry"] {
            let path = RelativePath::parse(Path::new(name)).expect("fixture path");
            for source in [
                b"(defun measure (value) value)".as_slice(),
                b"__kernel void measure(global float* values) {}",
                b"(define (measure value) value)",
            ] {
                let (_, signals) = classify(&path, source);
                assert!(
                    !signals
                        .iter()
                        .any(|signal| signal.language == "common-lisp"),
                    "{name}"
                );
            }
        }
    }

    #[test]
    fn common_lisp_content_preserves_independent_foreign_evidence() {
        let path = RelativePath::parse(Path::new("package.lisp")).expect("fixture path");
        let (_, signals) = classify(&path, b"(defpackage :sample) ; fn main(\n");
        for signal in [
            LanguageSignal {
                language: "common-lisp".to_owned(),
                evidence: LanguageEvidence::Extension,
            },
            LanguageSignal {
                language: "common-lisp".to_owned(),
                evidence: LanguageEvidence::Content,
            },
            LanguageSignal {
                language: "rust".to_owned(),
                evidence: LanguageEvidence::Content,
            },
        ] {
            assert!(signals.contains(&signal), "{signal:?}");
        }
        let path = RelativePath::parse(Path::new("entry")).expect("fixture path");
        let (_, signals) = classify(&path, b"#!/usr/bin/python3\n(defpackage :sample)\n");
        assert!(signals.contains(&LanguageSignal {
            language: "python".to_owned(),
            evidence: LanguageEvidence::Shebang
        }));
        assert!(
            !signals
                .iter()
                .any(|signal| signal.language == "common-lisp")
        );
    }

    #[test]
    fn common_lisp_content_is_bounded_and_retains_only_proved_utf8_headers() {
        let path = RelativePath::parse(Path::new("ambiguous.cl")).expect("fixture path");
        let expected = LanguageSignal {
            language: "common-lisp".to_owned(),
            evidence: LanguageEvidence::Content,
        };
        for (tail, remainder, recognized) in [
            ("(defpackag", "e :sample)", false),
            ("(defpackage :sample", ")", false),
            ("(defclass widget ", "() ())", false),
            ("(defclass widget (", ") ())", true),
        ] {
            let mut source = vec![b' '; MAX_CLASSIFICATION_BYTES - tail.len()];
            source.extend_from_slice(tail.as_bytes());
            source.extend_from_slice(remainder.as_bytes());
            let (_, signals) = classify(&path, &source);
            assert_eq!(signals.contains(&expected), recognized, "{tail}");
        }
        let mut source = b"(defpackage :sample)\n; ".to_vec();
        source.resize(MAX_CLASSIFICATION_BYTES - 1, b' ');
        source.extend_from_slice("é".as_bytes());
        let (_, signals) = classify(&path, &source);
        assert!(signals.contains(&expected));
        source[MAX_CLASSIFICATION_BYTES - 2] = 0xff;
        let (_, signals) = classify(&path, &source);
        assert!(!signals.contains(&expected));
        let (_, signals) = classify(&path, b"(defpackage :sample)\xc3");
        assert!(!signals.contains(&expected));
    }

    #[test]
    fn shebang_classification_recognizes_extensionless_perl_interpreters() {
        let path = RelativePath::parse(Path::new("runner")).expect("fixture path");
        for header in [
            "#!/usr/bin/perl",
            "#! /usr/local/bin/perl -w",
            "#!/usr/bin/perl5.42.2 -w",
            "#!/usr/bin/env perl",
            "#!/usr/bin/env -S perl -w",
            "#!/usr/bin/env --split-string=perl -w",
            "#!/usr/bin/env -S 'perl' -w",
            "#!/usr/bin/env -S \"perl\" -w",
            "#!/usr/bin/env -S -u UNUSED perl -w",
            "#!/usr/bin/env -S UNUSED=python perl -w",
        ] {
            let source = format!("{header}\nsub answer {{ return 42; }}\n");
            let (class, signals) = classify(&path, source.as_bytes());
            assert_eq!(class, InputClass::Source);
            assert_eq!(
                signals,
                [LanguageSignal {
                    language: "perl".to_owned(),
                    evidence: LanguageEvidence::Shebang,
                }],
                "{header}"
            );
        }
        let capability = language_capabilities()
            .iter()
            .find(|entry| entry.language == "perl")
            .expect("Perl capability");
        assert!(capability.detectors.contains(&"shebang"));
    }

    #[test]
    fn shared_pl_suffix_keeps_perl_and_prolog_evidence_distinct() {
        for path in ["facts.pl", "src/FACTS.PL"] {
            assert!(is_ambiguous_perl_prolog_source(path));
            let path = RelativePath::parse(Path::new(path)).expect("fixture path");
            let (_, signals) = classify(&path, b"% comment\nedge(a, b).\n");
            assert!(signals.contains(&LanguageSignal {
                language: "prolog".to_owned(),
                evidence: LanguageEvidence::Content,
            }));
            assert!(signals.contains(&LanguageSignal {
                language: "perl".to_owned(),
                evidence: LanguageEvidence::Extension,
            }));
        }
        for path in ["facts.pl.bak", "src/pl", "module.pm"] {
            assert!(!is_ambiguous_perl_prolog_source(path));
        }
        let path = RelativePath::parse(Path::new("script.pl")).expect("fixture path");
        let (_, signals) = classify(&path, b"use strict;\nsub edge { 1 }\n");
        assert_eq!(
            signals,
            vec![LanguageSignal {
                language: "perl".to_owned(),
                evidence: LanguageEvidence::Extension,
            }]
        );
        let capability = language_capabilities()
            .iter()
            .find(|entry| entry.language == "prolog")
            .expect("Prolog capability");
        assert_eq!(capability.detectors, ["content"]);
        assert_eq!(capability.analyzers, ["native"]);
    }

    #[test]
    fn prolog_dcg_content_keeps_independent_perl_extension_and_shebang_evidence() {
        for path in ["grammar.pl", "src/GRAMMAR.PL"] {
            let path = RelativePath::parse(Path::new(path)).expect("fixture path");
            for source in [
                "sentence(Token) --> [Token].\n",
                "#!/usr/bin/perl\nsentence(Token) --> [Token].\n",
            ] {
                let (_, signals) = classify(&path, source.as_bytes());
                assert!(signals.contains(&LanguageSignal {
                    language: "prolog".to_owned(),
                    evidence: LanguageEvidence::Content,
                }));
                assert!(signals.contains(&LanguageSignal {
                    language: "perl".to_owned(),
                    evidence: LanguageEvidence::Extension,
                }));
                assert_eq!(
                    signals.contains(&LanguageSignal {
                        language: "perl".to_owned(),
                        evidence: LanguageEvidence::Shebang,
                    }),
                    source.starts_with("#!"),
                );
            }
        }
    }

    #[test]
    fn prolog_dcg_content_cannot_use_an_arrow_or_body_start_outside_the_sample() {
        let path = RelativePath::parse(Path::new("grammar.pl")).expect("fixture path");
        for (tail, remainder, has_hint) in [
            ("sentence(Token) --", "> [Token].\n", false),
            ("sentence(Token) -->", " [Token].\n", false),
            ("sentence(Token) --> [", "Token].\n", true),
        ] {
            let mut source = vec![b' '; MAX_CLASSIFICATION_BYTES - tail.len()];
            source.extend_from_slice(tail.as_bytes());
            source.extend_from_slice(remainder.as_bytes());
            let (_, signals) = classify(&path, &source);
            assert_eq!(
                signals.contains(&LanguageSignal {
                    language: "prolog".to_owned(),
                    evidence: LanguageEvidence::Content,
                }),
                has_hint,
                "{tail}",
            );
            assert!(signals.contains(&LanguageSignal {
                language: "perl".to_owned(),
                evidence: LanguageEvidence::Extension,
            }));
        }
    }

    #[test]
    fn shebang_classification_uses_interpreter_identity_not_argument_substrings() {
        for (header, expected) in [
            ("#!/usr/bin/python3.12 -u", Some("python")),
            ("#!/usr/bin/env node", Some("javascript")),
            ("#!/usr/bin/env -S deno run", Some("javascript")),
            ("#!/bin/bash -e", Some("bash")),
            ("#!/usr/bin/env sh", Some("bash")),
            ("#!/usr/bin/perl python", Some("perl")),
            ("#!/opt/python-tools/bin/runner", None),
            ("#!/usr/bin/printf perl", None),
            ("#!/usr/bin/env -S printf python", None),
            ("#!/usr/bin/env -S --unset=perl printf", None),
            ("#!/usr/bin/env -S -u python printf", None),
            ("#!/usr/bin/env -S DATA=python printf", None),
            ("#!/usr/bin/env perl -w", None),
            ("#!/usr/bin/env -S perl-tool", None),
            ("#!/usr/bin/perl5..42", None),
            ("#!/usr/bin/perl5.42suffix", None),
            ("#!/usr/bin/env -S 'perl", None),
            ("#!/usr/bin/env -S ${RUNTIME}", None),
            ("#!/usr/bin/env -S --unknown perl", None),
            ("\n#!/usr/bin/perl", None),
        ] {
            assert_eq!(shebang_language(header.as_bytes()), expected, "{header}");
        }
        let mut source = b"#!/usr/bin/".to_vec();
        source.resize(MAX_CLASSIFICATION_BYTES, b'x');
        source.extend_from_slice(b"python\n");
        assert_eq!(shebang_language(&source), None);
        assert_eq!(shebang_language(b"#!/usr/bin/perl\xff\n"), None);
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
                "cache/kept.rs",
                "target/kept.rs"
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
    fn tracked_admission_reopens_only_required_artifact_routes() {
        let temporary = local_tempdir();
        write_fixture(&temporary, ".gitignore", b"!**/*.rs\n");
        let mut tracked = BTreeSet::new();
        let mut denied = Vec::new();
        for artifact in ["target", "node_modules", ".venv", "dist", "build"] {
            for prefix in [artifact.to_owned(), format!("fixtures/{artifact}")] {
                write_fixture(&temporary, &format!("{prefix}/.gitignore"), b"!**/*.rs\n");
                for suffix in ["kept.rs", "deep/kept.rs"] {
                    let path = format!("{prefix}/{suffix}");
                    write_fixture(&temporary, &path, b"fn value() {}\n");
                    tracked.insert(path);
                }
                for suffix in ["untracked.rs", "deep/untracked.rs"] {
                    let path = format!("{prefix}/{suffix}");
                    write_fixture(&temporary, &path, b"fn value() {}\n");
                    denied.push(path);
                }
                write_fixture(
                    &temporary,
                    &format!("{prefix}/untracked-dir/ignored.rs"),
                    b"fn value() {}\n",
                );
                denied.push(format!("{prefix}/untracked-dir"));
            }
        }
        let root = fixture_root(&temporary, b"tracked-artifact-routes");
        let policy = DiscoveryPolicy::build(Vec::new(), true)
            .expect("policy builds")
            .with_tracked_files(tracked.clone(), &Cancellation::new())
            .expect("tracked paths validate");
        let manifest = discover(&root, &config(), &policy, limits(), &Cancellation::new())
            .expect("tracked artifact discovery succeeds");
        tracked.insert(".gitignore".to_owned());
        assert_eq!(
            manifest
                .inputs
                .iter()
                .map(|input| input.path.clone())
                .collect::<BTreeSet<_>>(),
            tracked
        );
        for path in denied {
            assert!(
                manifest.exclusions.iter().any(|exclusion| {
                    exclusion.path == path && exclusion.reason == ExclusionReason::Policy
                }),
                "{path}"
            );
        }
    }

    #[test]
    fn tracked_artifact_admission_preserves_host_and_explicit_exclusions() {
        let temporary = local_tempdir();
        let paths = [
            "fixtures/target/kept.rs",
            "fixtures/target/host-denied.rs",
            "fixtures/target/repository-denied.rs",
            "fixtures/target/operation-denied.rs",
        ];
        for path in paths {
            write_fixture(&temporary, path, b"fn value() {}\n");
        }
        let root = fixture_root(&temporary, b"tracked-artifact-denials");
        let policy = DiscoveryPolicy::build(
            vec![
                PolicyRule {
                    layer: PolicyLayer::Default,
                    pattern: "**/host-denied.rs".to_owned(),
                    source: "host".to_owned(),
                },
                PolicyRule {
                    layer: PolicyLayer::Repository,
                    pattern: "**/repository-denied.rs".to_owned(),
                    source: "repository".to_owned(),
                },
                PolicyRule {
                    layer: PolicyLayer::Operation,
                    pattern: "**/operation-denied.rs".to_owned(),
                    source: "operation".to_owned(),
                },
            ],
            true,
        )
        .expect("policy builds")
        .with_tracked_files(paths.map(str::to_owned).into(), &Cancellation::new())
        .expect("tracked paths validate");
        let manifest = discover(&root, &config(), &policy, limits(), &Cancellation::new())
            .expect("tracked artifact discovery succeeds");
        assert_eq!(
            manifest
                .inputs
                .iter()
                .map(|input| input.path.as_str())
                .collect::<Vec<_>>(),
            ["fixtures/target/kept.rs"]
        );
        for path in paths.into_iter().skip(1) {
            assert!(manifest.exclusions.iter().any(|exclusion| {
                exclusion.path == path && exclusion.reason == ExclusionReason::Policy
            }));
        }
    }

    #[test]
    fn caller_default_exclusion_cannot_spoof_builtin_artifact_authority() {
        for source in ["host", "rootlight-default"] {
            let policy = DiscoveryPolicy::build(
                vec![PolicyRule {
                    layer: PolicyLayer::Default,
                    pattern: "target/".to_owned(),
                    source: source.to_owned(),
                }],
                true,
            )
            .expect("policy builds")
            .with_tracked_files(
                ["fixtures/target/kept.rs".to_owned()].into(),
                &Cancellation::new(),
            )
            .expect("tracked paths validate");
            for (path, is_directory) in [
                ("fixtures/target", true),
                ("fixtures/target/kept.rs", false),
            ] {
                let path = RelativePath::parse(Path::new(path)).expect("fixture path");
                let decision = policy.decision(&path, is_directory);
                assert!(
                    decision.excluded && !decision.included,
                    "{source}: {path:?}"
                );
            }
        }
    }

    #[test]
    fn tracked_admission_keeps_git_metadata_hard_under_pattern_negation() {
        let temporary = local_tempdir();
        let paths = [
            ".git",
            "nested/.git/target/kept.rs",
            "target/.git/build/kept.rs",
        ];
        for path in paths {
            write_fixture(&temporary, path, b"metadata\n");
        }
        let root = fixture_root(&temporary, b"tracked-metadata-boundary");
        let policy = DiscoveryPolicy::build(
            vec![
                PolicyRule {
                    layer: PolicyLayer::Default,
                    pattern: "!**/.git".to_owned(),
                    source: "host".to_owned(),
                },
                PolicyRule {
                    layer: PolicyLayer::Default,
                    pattern: "!**/target/".to_owned(),
                    source: "host".to_owned(),
                },
                PolicyRule {
                    layer: PolicyLayer::VcsIgnore,
                    pattern: "!**/.git/**".to_owned(),
                    source: "repository-ignore".to_owned(),
                },
                PolicyRule {
                    layer: PolicyLayer::Repository,
                    pattern: "!**".to_owned(),
                    source: "repository".to_owned(),
                },
                PolicyRule {
                    layer: PolicyLayer::Operation,
                    pattern: "!**".to_owned(),
                    source: "operation".to_owned(),
                },
            ],
            true,
        )
        .expect("policy builds")
        .with_tracked_files(paths.map(str::to_owned).into(), &Cancellation::new())
        .expect("tracked paths validate");
        for path in paths {
            let path = RelativePath::parse(Path::new(path)).expect("fixture path");
            let decision = policy.decision(&path, false);
            assert!(decision.excluded && !decision.included, "{path:?}");
            assert_eq!(
                decision.decisive_rule,
                Some(DecisiveRule {
                    source: "rootlight-default".to_owned(),
                    pattern: ".git".to_owned(),
                })
            );
        }
        let manifest = discover(&root, &config(), &policy, limits(), &Cancellation::new())
            .expect("metadata-safe discovery succeeds");
        assert!(manifest.inputs.is_empty());
        for path in [".git", "nested/.git", "target/.git"] {
            assert!(manifest.exclusions.iter().any(|exclusion| {
                exclusion.path == path && exclusion.reason == ExclusionReason::Policy
            }));
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_git_metadata_casing_is_hard_without_folding_source_policy() {
        for metadata_name in [".GIT", ".Git"] {
            let temporary = local_tempdir();
            let tracked_metadata = [
                format!("{metadata_name}/target/tracked.rs"),
                format!("worktree/{metadata_name}"),
                format!("target/nested/{metadata_name}/build/tracked.rs"),
            ];
            for path in &tracked_metadata {
                write_fixture(&temporary, path, b"gitdir: ../metadata\n");
            }
            for path in [
                format!("{metadata_name}/untracked.rs"),
                format!("target/nested/{metadata_name}/untracked.rs"),
            ] {
                write_fixture(&temporary, &path, b"metadata\n");
            }
            let source_cases = [
                (PolicyLayer::Default, "host", "Host"),
                (PolicyLayer::VcsIgnore, "vcs", "Vcs"),
                (PolicyLayer::Repository, "repository", "Repository"),
                (PolicyLayer::Operation, "operation", "Operation"),
            ];
            let mut rules = Vec::new();
            for (layer, excluded_name, retained_name) in source_cases {
                write_fixture(
                    &temporary,
                    &format!("src/{retained_name}.rs"),
                    b"fn value() {}\n",
                );
                for pattern in [
                    format!("!**/{metadata_name}"),
                    format!("!**/{metadata_name}/**"),
                    format!("src/{excluded_name}.rs"),
                ] {
                    rules.push(PolicyRule {
                        layer,
                        pattern,
                        source: excluded_name.to_owned(),
                    });
                }
            }
            for path in [
                "src/metadata-lookalike.git.rs",
                "artifact-case/Target/ordinary.rs",
                "target/kept.rs",
            ] {
                write_fixture(&temporary, path, b"fn value() {}\n");
            }
            let root = fixture_root(&temporary, b"windows-metadata-casing-boundary");
            let cancellation = Cancellation::new();
            for (directory, kind) in [
                (None, EntryKind::Directory),
                (Some("worktree"), EntryKind::File),
            ] {
                let directory = directory.map(|path| {
                    RelativePath::parse(Path::new(path)).expect("fixture directory path")
                });
                let entries = root
                    .read_directory(directory.as_ref(), 16, &cancellation)
                    .expect("fixture directory enumerates");
                assert!(entries.iter().any(|entry| {
                    entry.name == OsStr::new(metadata_name) && entry.kind == kind
                }));
            }
            let mut tracked = BTreeSet::from(tracked_metadata);
            tracked.insert("target/kept.rs".to_owned());
            let policy = DiscoveryPolicy::build(rules, true)
                .expect("policy builds")
                .with_tracked_files_in_root(&root, tracked, &cancellation)
                .expect("tracked paths validate");
            let manifest = discover(&root, &config(), &policy, limits(), &cancellation)
                .expect("metadata-safe discovery succeeds");
            assert_eq!(
                manifest
                    .inputs
                    .iter()
                    .map(|input| input.path.as_str())
                    .collect::<Vec<_>>(),
                [
                    "artifact-case/Target/ordinary.rs",
                    "src/Host.rs",
                    "src/Operation.rs",
                    "src/Repository.rs",
                    "src/Vcs.rs",
                    "src/metadata-lookalike.git.rs",
                    "target/kept.rs",
                ],
                "{metadata_name} metadata must not be indexed"
            );
            for path in [
                metadata_name.to_owned(),
                format!("worktree/{metadata_name}"),
                format!("target/nested/{metadata_name}"),
            ] {
                assert!(manifest.exclusions.iter().any(|exclusion| {
                    exclusion.path == path
                        && exclusion.reason == ExclusionReason::Policy
                        && exclusion.decisive_rule
                            == Some(DecisiveRule {
                                source: "rootlight-default".to_owned(),
                                pattern: ".git".to_owned(),
                            })
                }));
            }
            for path in [
                format!("{metadata_name}/target/tracked.rs"),
                format!("{metadata_name}/untracked.rs"),
                format!("target/nested/{metadata_name}/build/tracked.rs"),
                format!("target/nested/{metadata_name}/untracked.rs"),
                format!("worktree/{metadata_name}"),
            ] {
                let path = RelativePath::parse(Path::new(&path)).expect("fixture metadata path");
                let decision = policy.decision(&path, false);
                assert!(decision.excluded && !decision.included, "{path:?}");
            }
            for (_, excluded_name, retained_name) in source_cases {
                let excluded = RelativePath::parse(Path::new(&format!("src/{excluded_name}.rs")))
                    .expect("excluded source path");
                let retained = RelativePath::parse(Path::new(&format!("src/{retained_name}.rs")))
                    .expect("retained source path");
                assert!(policy.decision(&excluded, false).excluded);
                assert!(!policy.decision(&retained, false).excluded);
            }
            let artifact = RelativePath::parse(Path::new("artifact-case/target/ordinary.rs"))
                .expect("case-sensitive artifact policy path");
            assert!(policy.decision(&artifact, false).excluded);
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn git_metadata_policy_preserves_distinct_non_windows_names() {
        let policy = DiscoveryPolicy::build(Vec::new(), true).expect("policy builds");
        for name in [".GIT", ".Git", "src/.GIT", "src/.Git"] {
            let path = RelativePath::parse(Path::new(name)).expect("distinct metadata-like path");
            for is_directory in [false, true] {
                assert!(!policy.decision(&path, is_directory).excluded);
            }
        }
        for name in [".git", "src/.git", "src/.git/tracked.rs"] {
            let path = RelativePath::parse(Path::new(name)).expect("metadata path");
            for is_directory in [false, true] {
                assert!(policy.decision(&path, is_directory).excluded);
            }
        }
    }

    #[test]
    fn tracked_artifact_admission_preserves_binary_size_and_depth_safety() {
        let temporary = local_tempdir();
        write_fixture(&temporary, "target/kept.rs", b"fn value() {}\n");
        write_fixture(&temporary, "target/binary.rs", b"\0binary\0");
        write_fixture(&temporary, "target/large.rs", &[b'x'; 32]);
        write_fixture(&temporary, "target/deep/kept.rs", b"fn value() {}\n");
        let root = fixture_root(&temporary, b"tracked-artifact-safety");
        let policy = DiscoveryPolicy::build(Vec::new(), true)
            .expect("policy builds")
            .with_tracked_files(
                [
                    "target/kept.rs",
                    "target/binary.rs",
                    "target/large.rs",
                    "target/deep/kept.rs",
                ]
                .map(str::to_owned)
                .into(),
                &Cancellation::new(),
            )
            .expect("tracked paths validate");
        let bounds = DiscoveryLimits::new(100, 1, 16, 16).expect("fixture bounds");
        let manifest = discover(&root, &config(), &policy, bounds, &Cancellation::new())
            .expect("bounded tracked discovery succeeds");
        assert_eq!(
            manifest
                .inputs
                .iter()
                .map(|input| input.path.as_str())
                .collect::<Vec<_>>(),
            ["target/kept.rs"]
        );
        for (path, reason) in [
            ("target/binary.rs", ExclusionReason::Binary),
            ("target/large.rs", ExclusionReason::Oversized),
            ("target/deep", ExclusionReason::DepthLimit),
        ] {
            assert!(
                manifest
                    .exclusions
                    .iter()
                    .any(|exclusion| { exclusion.path == path && exclusion.reason == reason })
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn tracked_artifact_admission_does_not_follow_symbolic_links() {
        use std::os::unix::fs::symlink;

        let temporary = local_tempdir();
        write_fixture(&temporary, "target/source.rs", b"fn value() {}\n");
        symlink("source.rs", temporary.path().join("target/linked.rs"))
            .expect("tracked fixture link is created");
        let root = fixture_root(&temporary, b"tracked-artifact-links");
        let policy = DiscoveryPolicy::build(Vec::new(), true)
            .expect("policy builds")
            .with_tracked_files(["target/linked.rs".to_owned()].into(), &Cancellation::new())
            .expect("tracked paths validate");
        let manifest = discover(&root, &config(), &policy, limits(), &Cancellation::new())
            .expect("link-safe tracked discovery succeeds");
        assert!(manifest.inputs.is_empty());
        assert!(manifest.exclusions.iter().any(|exclusion| {
            exclusion.path == "target/linked.rs" && exclusion.reason == ExclusionReason::Link
        }));
    }

    #[cfg(windows)]
    #[test]
    fn tracked_admission_resolves_existing_casing_without_admitting_siblings() {
        let temporary = local_tempdir();
        write_fixture(&temporary, ".gitignore", b"[Cc]ache/\n*.fs\n");
        for path in [
            "Cache/Deep/Unit.fs",
            "Cache/Deep/Untracked.fs",
            "Cache/Deep/Denied.fs",
            "Cache/Deep/RepositoryDenied.fs",
            "target/Kept.fs",
            "fixtures/target/Deep/Unit.fs",
            "fixtures/target/Deep/Untracked.fs",
            "fixtures/target/Deep/Denied.fs",
            "fixtures/target/Deep/RepositoryDenied.fs",
        ] {
            write_fixture(&temporary, path, b"module Unit\nlet value = 1\n");
        }
        std::fs::hard_link(
            temporary.path().join("Cache/Deep/Unit.fs"),
            temporary.path().join("Cache/Deep/UntrackedLink.fs"),
        )
        .expect("untracked hardlink is created");
        std::fs::hard_link(
            temporary.path().join("fixtures/target/Deep/Unit.fs"),
            temporary
                .path()
                .join("fixtures/target/Deep/UntrackedLink.fs"),
        )
        .expect("untracked artifact hardlink is created");
        let root = fixture_root(&temporary, b"tracked-path-casing");
        let policy = DiscoveryPolicy::build(
            vec![
                PolicyRule {
                    layer: PolicyLayer::Repository,
                    pattern: "**/RepositoryDenied.fs".to_owned(),
                    source: "repository".to_owned(),
                },
                PolicyRule {
                    layer: PolicyLayer::Operation,
                    pattern: "**/Denied.fs".to_owned(),
                    source: "operation".to_owned(),
                },
            ],
            true,
        )
        .expect("policy builds")
        .with_tracked_files_in_root(
            &root,
            [
                "cache/deep/unit.fs",
                "cache/deep/denied.fs",
                "cache/deep/repositorydenied.fs",
                "target/kept.fs",
                "cache/deep/missing.fs",
                "fixtures/target/deep/unit.fs",
                "fixtures/target/deep/denied.fs",
                "fixtures/target/deep/repositorydenied.fs",
            ]
            .map(str::to_owned)
            .into(),
            &Cancellation::new(),
        )
        .expect("tracked paths validate");
        let config = ConfigSnapshot::resolve(&[rootlight_config::ConfigLayer {
            source: rootlight_config::ConfigSource::User,
            contents: "version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.paths]\n'Cache/Deep/Unit.fs'='actual-configuration'\n'cache/deep/unit.fs'='lowercase-configuration'\n",
        }]).expect("exact source-language configuration resolves");
        let manifest = discover(&root, &config, &policy, limits(), &Cancellation::new())
            .expect("tracked discovery succeeds");
        assert_eq!(
            manifest
                .inputs
                .iter()
                .map(|input| input.path.as_str())
                .collect::<Vec<_>>(),
            [
                ".gitignore",
                "Cache/Deep/Unit.fs",
                "fixtures/target/Deep/Unit.fs",
                "target/Kept.fs"
            ]
        );
        let retained = manifest
            .inputs
            .iter()
            .find(|input| input.path == "Cache/Deep/Unit.fs")
            .expect("existing spelling is retained");
        let actual = RelativePath::parse(Path::new("Cache/Deep/Unit.fs")).expect("actual path");
        assert_eq!(retained.file, root.file_id(&actual));
        assert!(retained.language_signals.iter().any(|signal| {
            signal.language == "actual-configuration"
                && signal.evidence == LanguageEvidence::Configuration
        }));
        assert!(
            !retained
                .language_signals
                .iter()
                .any(|signal| signal.language == "lowercase-configuration")
        );
        assert!(policy.admits_tracked_path(
            &RelativePath::parse(Path::new("cache/deep/missing.fs")).expect("missing exact key"),
            false,
        ));
        assert!(
            !policy.admits_tracked_path(
                &RelativePath::parse(Path::new("Cache/Deep/Missing.fs"))
                    .expect("unresolved alternate key"),
                false,
            )
        );
        assert!(!policy.admits_tracked_path(
            &RelativePath::parse(Path::new("Cache/Deep/UntrackedLink.fs")).expect("hardlink path"),
            false,
        ));
        assert!(
            !policy.admits_tracked_path(
                &RelativePath::parse(Path::new("fixtures/target/Deep/UntrackedLink.fs"))
                    .expect("artifact hardlink path"),
                false,
            )
        );
        let artifact = manifest
            .inputs
            .iter()
            .find(|input| input.path == "fixtures/target/Deep/Unit.fs")
            .expect("artifact source existing spelling is retained");
        let artifact_path = RelativePath::parse(Path::new("fixtures/target/Deep/Unit.fs"))
            .expect("artifact source path");
        assert_eq!(artifact.file, root.file_id(&artifact_path));
    }

    #[test]
    fn tracked_capability_admission_rejects_invalid_input_and_cancellation() {
        let temporary = local_tempdir();
        let root = fixture_root(&temporary, b"tracked-input-validation");
        for invalid in ["../escape.fs", "/absolute.fs", "alias\\file.fs", ""] {
            assert!(
                DiscoveryPolicy::build(Vec::new(), false)
                    .expect("policy builds")
                    .with_tracked_files_in_root(
                        &root,
                        [invalid.to_owned()].into(),
                        &Cancellation::new()
                    )
                    .is_err()
            );
        }
        let cancellation = Cancellation::new();
        assert!(cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest));
        assert!(matches!(
            DiscoveryPolicy::build(Vec::new(), false)
                .expect("policy builds")
                .with_tracked_files_in_root(&root, ["missing.fs".to_owned()].into(), &cancellation),
            Err(DiscoveryError::Cancelled(_)),
        ));
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires NTFS directory case-sensitivity setup permission"]
    fn tracked_admission_keeps_case_sensitive_objects_and_hardlinks_distinct() {
        let temporary = local_tempdir();
        let sensitive = temporary.path().join("Sensitive");
        std::fs::create_dir(&sensitive).expect("empty fixture directory exists");
        let setup = std::process::Command::new("fsutil.exe")
            .args(["file", "setCaseSensitiveInfo"])
            .arg(&sensitive)
            .arg("enable")
            .output()
            .expect("Windows filesystem utility starts");
        assert!(
            setup.status.success(),
            "case-sensitive fixture setup requires permission"
        );
        write_fixture(&temporary, ".gitignore", b"Sensitive/\n");
        for (path, source) in [
            ("Sensitive/Unit.fs", b"module Upper\n".as_slice()),
            ("Sensitive/unit.fs", b"module Lower\n".as_slice()),
            ("Sensitive/missing.fs", b"module Missing\n".as_slice()),
        ] {
            write_fixture(&temporary, path, source);
        }
        assert_ne!(
            std::fs::read(sensitive.join("Unit.fs")).unwrap(),
            std::fs::read(sensitive.join("unit.fs")).unwrap(),
        );
        for alias in ["Alias.fs", "alias.fs"] {
            std::fs::hard_link(sensitive.join("Unit.fs"), sensitive.join(alias))
                .expect("case-distinct hardlink is created");
        }
        let root = fixture_root(&temporary, b"case-sensitive-tracked-admission");
        let policy = DiscoveryPolicy::build(Vec::new(), true)
            .expect("policy builds")
            .with_tracked_files_in_root(
                &root,
                [
                    "Sensitive/Unit.fs",
                    "Sensitive/Alias.fs",
                    "Sensitive/Missing.fs",
                ]
                .map(str::to_owned)
                .into(),
                &Cancellation::new(),
            )
            .expect("tracked paths validate");
        let manifest = discover(&root, &config(), &policy, limits(), &Cancellation::new())
            .expect("discovery succeeds");
        assert_eq!(
            manifest
                .inputs
                .iter()
                .map(|input| input.path.as_str())
                .collect::<Vec<_>>(),
            [".gitignore", "Sensitive/Alias.fs", "Sensitive/Unit.fs"]
        );
        for untracked in [
            "Sensitive/unit.fs",
            "Sensitive/alias.fs",
            "Sensitive/missing.fs",
        ] {
            assert!(!policy.admits_tracked_path(
                &RelativePath::parse(Path::new(untracked)).expect("untracked path"),
                false,
            ));
        }
        assert!(
            root.resolve_regular_path(
                &RelativePath::parse(Path::new("Sensitive/Missing.fs"))
                    .expect("missing uppercase path"),
                &Cancellation::new(),
            )
            .is_err()
        );
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
