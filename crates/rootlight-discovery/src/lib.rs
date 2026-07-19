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
use rootlight_config::ConfigSnapshot;
use rootlight_ids::{ContentHash, FileId, RepositoryId, content_hash};
use rootlight_incremental::IncrementalError;
use rootlight_vfs::{
    DirectoryEntry, EntryKind, RelativePath, RepositoryRoot, SourceSnapshot, VfsError,
};
use serde::{Deserialize, Serialize};

mod incremental;

pub use incremental::{
    IncrementalDiscovery, IncrementalDiscoveryBaseline, IncrementalDiscoveryContext,
    correlate_incremental_manifest, discover_incremental,
};

/// Initial deterministic discovery-manifest version.
pub const DISCOVERY_MANIFEST_VERSION: &str = "1.0";
/// Hard entry ceiling independent of caller configuration.
pub const MAX_DISCOVERY_ENTRIES: usize = 1_000_000;
/// Hard traversal-depth ceiling independent of caller configuration.
pub const MAX_DISCOVERY_DEPTH: usize = 256;
/// Hard diagnostic ceiling independent of caller configuration.
pub const MAX_DISCOVERY_DIAGNOSTICS: usize = 10_000;
/// Maximum bytes read for bounded content classification.
pub const MAX_CLASSIFICATION_BYTES: usize = 8 * 1024;

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
        Self {
            max_entries: MAX_DISCOVERY_ENTRIES.min(100_000),
            max_depth: MAX_DISCOVERY_DEPTH.min(128),
            max_file_bytes: analysis
                .max_source_file_bytes
                .min(rootlight_vfs::MAX_SNAPSHOT_BYTES),
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
        })
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
        let mut decision = PolicyDecision::default();
        for matcher in [&self.matchers.default, &self.matchers.vcs_ignore] {
            if let Some(layer_decision) =
                decision_from_match(matcher.matched(candidate, is_directory), self.audit)
            {
                decision = layer_decision;
            }
        }
        if let Some(scoped_decision) =
            scoped_ignores.and_then(|ignores| ignores.decision(path, is_directory, self.audit))
        {
            decision = scoped_decision;
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
        let candidate = Path::new(path.as_str());
        let mut scope = parent_scope(path.as_str());
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
    /// Well-known language manifest name.
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
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryCoverage {
    /// Total filesystem entries observed.
    pub visited: u64,
    /// Included regular inputs.
    pub included: u64,
    /// Exclusions grouped by stable reason name.
    pub excluded: BTreeMap<String, u64>,
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
    let mut state = DiscoveryState::new(root, config, policy, limits, cancellation);
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
}

impl<'a> DiscoveryState<'a> {
    fn new(
        root: &'a RepositoryRoot,
        config: &'a ConfigSnapshot,
        policy: &'a DiscoveryPolicy,
        limits: DiscoveryLimits,
        cancellation: &'a Cancellation,
    ) -> Self {
        let mut queue = VecDeque::new();
        queue.push_back((None, 0));
        Self {
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
        }
    }

    fn run(&mut self) -> Result<(), DiscoveryError> {
        while let Some((directory, depth)) = self.queue.pop_front() {
            self.cancellation.check()?;
            let entries = self.root.read_directory(directory.as_ref())?;
            self.cancellation.check()?;
            self.ensure_entry_capacity(entries.len())?;
            // Ignore files become readable only after ancestor policy admits this
            // directory, so descendant negations cannot force traversal into it.
            let mut ignore_snapshot = self.load_scoped_ignore(directory.as_ref(), &entries)?;
            for entry in entries {
                self.cancellation.check()?;
                let cached_snapshot = if entry.name == OsStr::new(".gitignore") {
                    ignore_snapshot.take()
                } else {
                    None
                };
                self.visit_entry(directory.as_ref(), depth, entry, cached_snapshot)?;
            }
        }
        Ok(())
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
        let snapshot = self.root.snapshot_with_cancellation(
            &path,
            self.limits.max_file_bytes,
            self.cancellation,
        )?;
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
            EntryKind::File => {
                self.visit_file(path, entry.length, decision.decisive_rule, cached_snapshot)?
            }
        }
        Ok(())
    }

    fn visit_file(
        &mut self,
        path: RelativePath,
        observed_length: u64,
        decisive_rule: Option<DecisiveRule>,
        cached_snapshot: Option<SourceSnapshot>,
    ) -> Result<(), DiscoveryError> {
        if observed_length > self.limits.max_file_bytes {
            self.exclude(&path, ExclusionReason::Oversized, decisive_rule);
            return Ok(());
        }
        let snapshot_result = match cached_snapshot {
            Some(snapshot) => Ok(snapshot),
            None => self.root.snapshot_with_cancellation(
                &path,
                self.limits.max_file_bytes,
                self.cancellation,
            ),
        };
        let snapshot = match snapshot_result {
            Ok(snapshot) => snapshot,
            Err(VfsError::FileTooLarge { .. }) => {
                self.exclude(&path, ExclusionReason::Oversized, decisive_rule);
                return Ok(());
            }
            Err(VfsError::LinkedPath | VfsError::OpenFile { .. }) => {
                self.exclude(&path, ExclusionReason::Unreadable, decisive_rule);
                self.diagnostic(Some(&path), "DISCOVERY_UNREADABLE");
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        if looks_binary(snapshot.content()) {
            self.exclude(&path, ExclusionReason::Binary, decisive_rule);
            return Ok(());
        }
        let (class, language_signals) = classify(&path, snapshot.content());
        self.inputs.push(ManifestInput {
            file: snapshot.file(),
            path: path.as_str().to_owned(),
            content_hash: snapshot.content_hash(),
            bytes: snapshot.metadata().length,
            class,
            language_signals,
            decisive_rule,
        });
        self.coverage.included = self.coverage.included.saturating_add(1);
        Ok(())
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

    fn finish(mut self) -> DiscoveryManifest {
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
        DiscoveryManifest {
            version: DISCOVERY_MANIFEST_VERSION.to_owned(),
            repository: self.root.repository(),
            configuration_hash: self.config.hash(),
            inputs: self.inputs,
            exclusions: self.exclusions,
            diagnostics: self.diagnostics,
            coverage: self.coverage,
        }
    }
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

fn extension_language(path: &str) -> Option<&'static str> {
    for (suffix, language) in [
        (".d.ts", "typescript"),
        (".blade.php", "php"),
        (".pb.go", "go"),
        (".rs", "rust"),
        (".tsx", "typescript"),
        (".ts", "typescript"),
        (".jsx", "javascript"),
        (".js", "javascript"),
        (".py", "python"),
        (".go", "go"),
        (".java", "java"),
        (".cs", "csharp"),
        (".php", "php"),
    ] {
        if path.ends_with(suffix) {
            return Some(language);
        }
    }
    None
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
        Some("shell")
    } else {
        None
    }
}

fn content_language(content: &[u8]) -> Option<&'static str> {
    let sample = content.get(..content.len().min(MAX_CLASSIFICATION_BYTES))?;
    let text = String::from_utf8_lossy(sample);
    if text.contains("fn main(") || text.contains("pub struct ") {
        Some("rust")
    } else if text.contains("package main") && text.contains("func ") {
        Some("go")
    } else if text.contains("def ") && text.contains(':') {
        Some("python")
    } else {
        None
    }
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
        assert_eq!(
            legacy_limits.max_file_bytes,
            rootlight_config::DEFAULT_MAX_SOURCE_FILE_BYTES
        );
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
    fn language_and_input_classification_uses_multiple_evidence_kinds() {
        let (class, signals) = classify(
            &RelativePath::parse(Path::new("generated/api.d.ts")).expect("fixture path is valid"),
            b"// generated file; do not edit\nexport interface Api {}",
        );
        assert_eq!(class, InputClass::Generated);
        assert!(signals.iter().any(|signal| {
            signal.language == "typescript" && signal.evidence == LanguageEvidence::Extension
        }));
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
