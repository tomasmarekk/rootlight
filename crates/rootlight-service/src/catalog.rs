//! Immutable, bounded repository-catalog snapshots for stable pagination.
//!
//! The store freezes filtered records on the first page so later catalog
//! mutations cannot change the membership or ordering of a cursor session.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt,
};

use rootlight_ids::{ContentHash, GenerationId, RepositoryId, content_hash};
use rootlight_ir::{AnalysisTier, CoverageStatus};
use unicode_casefold::UnicodeCaseFold as _;
use unicode_normalization::UnicodeNormalization as _;

/// Current binary encoding version for [`CatalogSortKey`].
pub const CATALOG_SORT_VERSION: u16 = 1;
/// Maximum public repository display-name or alias size in UTF-8 bytes.
pub const CATALOG_MAX_LABEL_BYTES: usize = 256;
/// Maximum UTF-8 byte length of one display-only canonical repository path.
pub const CATALOG_MAX_ROOT_PATH_BYTES: usize = 32 * 1024;
/// Maximum normalized query or sort-name size in UTF-8 bytes.
pub const CATALOG_MAX_NORMALIZED_TEXT_BYTES: usize = 1_024;
/// Maximum encoded byte length of one version-1 sort key.
pub const CATALOG_MAX_SORT_KEY_BYTES: usize =
    size_of::<u16>() + CATALOG_MAX_NORMALIZED_TEXT_BYTES + 16;
/// Maximum number of language coverage entries retained per repository.
pub const CATALOG_MAX_LANGUAGES: usize = 64;
/// Maximum UTF-8 byte length of a normalized language identifier.
pub const CATALOG_MAX_LANGUAGE_BYTES: usize = 64;
/// Maximum repositories returned by one page.
pub const CATALOG_MAX_PAGE_SIZE: u16 = 200;
/// Default maximum number of simultaneously retained snapshots.
pub const CATALOG_MAX_SNAPSHOTS: usize = 64;
/// Default maximum entries retained by one snapshot.
pub const CATALOG_MAX_ENTRIES_PER_SNAPSHOT: usize = 4_096;
/// Default maximum entries retained across all snapshots.
pub const CATALOG_MAX_RETAINED_ENTRIES: usize = 16_384;
/// Default maximum logical record bytes retained across all snapshots.
pub const CATALOG_MAX_RETAINED_BYTES: usize = 16 * 1024 * 1024;
/// Public continuation-cursor lifetime that catalog snapshots must exceed.
pub const CATALOG_CURSOR_TTL_MILLIS: u64 = 5 * 60 * 1_000;
/// Default snapshot lifetime in caller-supplied monotonic milliseconds.
///
/// This strictly exceeds the public five-minute cursor lifetime, so a cursor
/// cannot normally outlive the immutable state it authenticates.
pub const CATALOG_SNAPSHOT_TTL_MILLIS: u64 = 10 * 60 * 1_000;

const SNAPSHOT_ID_CONTEXT: &[u8] = b"rootlight.repository-catalog.snapshot/1";
const MAX_QUERY_CHARACTERS: usize = 256;
const MAX_TOMBSTONES_MULTIPLIER: usize = 4;

/// Opaque identity of one immutable repository-catalog snapshot.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CatalogSnapshotId(ContentHash);

impl CatalogSnapshotId {
    /// Creates an identity from its canonical 32-byte digest.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(ContentHash::from_bytes(bytes))
    }

    /// Returns the canonical 32-byte digest.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }
}

impl fmt::Debug for CatalogSnapshotId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("CatalogSnapshotId")
            .field(&self.0.to_string())
            .finish()
    }
}

impl fmt::Display for CatalogSnapshotId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Caller-supplied monotonic time used for deterministic snapshot expiry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CatalogInstant(u64);

impl CatalogInstant {
    /// Creates a monotonic timestamp from milliseconds in the caller's epoch.
    #[must_use]
    pub const fn from_millis(milliseconds: u64) -> Self {
        Self(milliseconds)
    }

    /// Returns milliseconds in the caller's monotonic epoch.
    #[must_use]
    pub const fn as_millis(self) -> u64 {
        self.0
    }
}

/// Hard retention limits for [`CatalogSnapshotStore`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogSnapshotLimits {
    maximum_snapshots: usize,
    maximum_entries_per_snapshot: usize,
    maximum_retained_entries: usize,
    maximum_retained_bytes: usize,
    ttl_millis: u64,
}

impl CatalogSnapshotLimits {
    /// Creates checked hard limits for a snapshot store.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError::InvalidLimits`] when a limit is zero, exceeds
    /// the service hard ceiling, is internally inconsistent, or the snapshot
    /// lifetime does not strictly exceed [`CATALOG_CURSOR_TTL_MILLIS`].
    pub const fn new(
        maximum_snapshots: usize,
        maximum_entries_per_snapshot: usize,
        maximum_retained_entries: usize,
        maximum_retained_bytes: usize,
        ttl_millis: u64,
    ) -> Result<Self, CatalogError> {
        if maximum_snapshots == 0
            || maximum_snapshots > CATALOG_MAX_SNAPSHOTS
            || maximum_entries_per_snapshot == 0
            || maximum_entries_per_snapshot > CATALOG_MAX_ENTRIES_PER_SNAPSHOT
            || maximum_retained_entries < maximum_entries_per_snapshot
            || maximum_retained_entries > CATALOG_MAX_RETAINED_ENTRIES
            || maximum_retained_bytes == 0
            || maximum_retained_bytes > CATALOG_MAX_RETAINED_BYTES
            || ttl_millis <= CATALOG_CURSOR_TTL_MILLIS
        {
            return Err(CatalogError::InvalidLimits);
        }
        Ok(Self {
            maximum_snapshots,
            maximum_entries_per_snapshot,
            maximum_retained_entries,
            maximum_retained_bytes,
            ttl_millis,
        })
    }
}

impl Default for CatalogSnapshotLimits {
    fn default() -> Self {
        Self {
            maximum_snapshots: CATALOG_MAX_SNAPSHOTS,
            maximum_entries_per_snapshot: CATALOG_MAX_ENTRIES_PER_SNAPSHOT,
            maximum_retained_entries: CATALOG_MAX_RETAINED_ENTRIES,
            maximum_retained_bytes: CATALOG_MAX_RETAINED_BYTES,
            ttl_millis: CATALOG_SNAPSHOT_TTL_MILLIS,
        }
    }
}

/// Canonical lifecycle state for one repository catalog record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum CatalogRepositoryState {
    /// Repository is indexed and queryable.
    Ready,
    /// An indexing operation is in progress.
    Indexing,
    /// Repository is queryable with reduced capability.
    Degraded,
    /// Index integrity checks failed.
    Corrupt,
    /// A catalog or generation migration is required.
    MigrationRequired,
    /// A complete repository rebuild is required.
    RebuildRequired,
}

impl CatalogRepositoryState {
    /// Returns the stable public state label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Indexing => "indexing",
            Self::Degraded => "degraded",
            Self::Corrupt => "corrupt",
            Self::MigrationRequired => "migration_required",
            Self::RebuildRequired => "rebuild_required",
        }
    }
}

/// Freshness of one structural or semantic repository view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CatalogFreshness {
    /// Matches the latest successfully committed authoritative scan.
    Current,
    /// Queryable, but a newer generation is active.
    Superseded,
    /// Queryable with a known stale source snapshot.
    Stale,
}

impl CatalogFreshness {
    /// Returns the stable public freshness label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Superseded => "superseded",
            Self::Stale => "stale",
        }
    }
}

/// Receipt-derived coverage for one normalized language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogLanguageCoverage {
    language: String,
    tier: AnalysisTier,
    status: CoverageStatus,
    discovered_files: u64,
    indexed_files: u64,
}

impl CatalogLanguageCoverage {
    /// Creates a checked language coverage entry.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError::InvalidLanguage`] when the identifier is empty,
    /// oversized, noncanonical, or contains characters outside lowercase ASCII
    /// letters, digits, `+`, `-`, `_`, or `.`.
    pub fn new(
        language: String,
        tier: AnalysisTier,
        status: CoverageStatus,
        discovered_files: u64,
        indexed_files: u64,
    ) -> Result<Self, CatalogError> {
        if language.is_empty()
            || language.len() > CATALOG_MAX_LANGUAGE_BYTES
            || !language.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'+' | b'-' | b'_' | b'.')
            })
        {
            return Err(CatalogError::InvalidLanguage);
        }
        if indexed_files > discovered_files {
            return Err(CatalogError::InvalidCoverage);
        }
        Ok(Self {
            language,
            tier,
            status,
            discovered_files,
            indexed_files,
        })
    }

    /// Returns the normalized language identifier.
    #[must_use]
    pub fn language(&self) -> &str {
        &self.language
    }

    /// Returns the observed analysis tier.
    #[must_use]
    pub const fn tier(&self) -> AnalysisTier {
        self.tier
    }

    /// Returns the observed coverage status.
    #[must_use]
    pub const fn status(&self) -> CoverageStatus {
        self.status
    }

    /// Returns discovered files in this language.
    #[must_use]
    pub const fn discovered_files(&self) -> u64 {
        self.discovered_files
    }

    /// Returns indexed files in this language.
    #[must_use]
    pub const fn indexed_files(&self) -> u64 {
        self.indexed_files
    }
}

/// Authoritative repository metadata frozen into list snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogRepositoryRecord {
    repository: RepositoryId,
    display_name: String,
    alias: Option<String>,
    root_path: Option<String>,
    active_generation: Option<GenerationId>,
    generation_count: u64,
    state: CatalogRepositoryState,
    structural_freshness: CatalogFreshness,
    semantic_freshness: CatalogFreshness,
    coverage: Vec<CatalogLanguageCoverage>,
}

impl CatalogRepositoryRecord {
    /// Creates a checked repository record without an alias or coverage.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError::InvalidLabel`] for an unsafe display label or
    /// [`CatalogError::InvalidGenerationState`] when an active generation is
    /// paired with a zero generation count.
    pub fn new(
        repository: RepositoryId,
        display_name: String,
        active_generation: Option<GenerationId>,
        generation_count: u64,
        state: CatalogRepositoryState,
    ) -> Result<Self, CatalogError> {
        validate_label(&display_name)?;
        if active_generation.is_some() && generation_count == 0 {
            return Err(CatalogError::InvalidGenerationState);
        }
        Ok(Self {
            repository,
            display_name,
            alias: None,
            root_path: None,
            active_generation,
            generation_count,
            state,
            structural_freshness: CatalogFreshness::Current,
            semantic_freshness: CatalogFreshness::Current,
            coverage: Vec::new(),
        })
    }

    /// Attaches a checked authoritative alias.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError::InvalidLabel`] when the alias is empty,
    /// oversized, or contains path separators or control characters.
    pub fn with_alias(mut self, alias: Option<String>) -> Result<Self, CatalogError> {
        if let Some(alias) = &alias {
            validate_label(alias)?;
        }
        self.alias = alias;
        Ok(self)
    }

    /// Attaches the display-only canonical repository root.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError::InvalidLabel`] when the path is empty,
    /// oversized, or contains control characters.
    pub fn with_root_path(mut self, root_path: Option<String>) -> Result<Self, CatalogError> {
        if root_path.as_deref().is_some_and(|path| {
            path.is_empty()
                || path.len() > CATALOG_MAX_ROOT_PATH_BYTES
                || path.chars().any(char::is_control)
        }) {
            return Err(CatalogError::InvalidLabel);
        }
        self.root_path = root_path;
        Ok(self)
    }

    /// Attaches structural and semantic freshness.
    #[must_use]
    pub const fn with_freshness(
        mut self,
        structural: CatalogFreshness,
        semantic: CatalogFreshness,
    ) -> Self {
        self.structural_freshness = structural;
        self.semantic_freshness = semantic;
        self
    }

    /// Attaches deterministic, unique language coverage.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError::TooManyLanguages`] above the hard bound or
    /// [`CatalogError::DuplicateLanguage`] when a language appears twice.
    pub fn with_coverage(
        mut self,
        mut coverage: Vec<CatalogLanguageCoverage>,
    ) -> Result<Self, CatalogError> {
        if coverage.len() > CATALOG_MAX_LANGUAGES {
            return Err(CatalogError::TooManyLanguages);
        }
        coverage.sort_by(|left, right| left.language.cmp(&right.language));
        if coverage
            .windows(2)
            .any(|pair| pair[0].language == pair[1].language)
        {
            return Err(CatalogError::DuplicateLanguage);
        }
        self.coverage = coverage;
        Ok(self)
    }

    /// Returns the stable repository identity.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Returns the source-free display label captured at registration.
    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Returns the authoritative alias, when configured.
    #[must_use]
    pub fn alias(&self) -> Option<&str> {
        self.alias.as_deref()
    }

    /// Returns the display-only canonical repository root, when retained.
    #[must_use]
    pub fn root_path(&self) -> Option<&str> {
        self.root_path.as_deref()
    }

    /// Returns the active immutable generation, when published.
    #[must_use]
    pub const fn active_generation(&self) -> Option<GenerationId> {
        self.active_generation
    }

    /// Returns the number of generations published for this repository.
    #[must_use]
    pub const fn generation_count(&self) -> u64 {
        self.generation_count
    }

    /// Returns the canonical repository lifecycle state.
    #[must_use]
    pub const fn state(&self) -> CatalogRepositoryState {
        self.state
    }

    /// Returns structural freshness.
    #[must_use]
    pub const fn structural_freshness(&self) -> CatalogFreshness {
        self.structural_freshness
    }

    /// Returns semantic freshness.
    #[must_use]
    pub const fn semantic_freshness(&self) -> CatalogFreshness {
        self.semantic_freshness
    }

    /// Returns deterministic language coverage sorted by language.
    #[must_use]
    pub fn coverage(&self) -> &[CatalogLanguageCoverage] {
        &self.coverage
    }

    /// Returns indexed languages in the same order as [`Self::coverage`].
    pub fn languages(&self) -> impl ExactSizeIterator<Item = &str> {
        self.coverage.iter().map(|entry| entry.language.as_str())
    }
}

/// Canonical repository-list filter frozen into a snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogListFilter {
    query: Option<String>,
    states: Option<Vec<CatalogRepositoryState>>,
}

impl CatalogListFilter {
    /// Normalizes a public query and canonicalizes repository state filters.
    ///
    /// Workspace filtering is reserved for a future authoritative workspace
    /// catalog and is rejected rather than ignored.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError::InvalidQuery`] for an oversized query or
    /// [`CatalogError::UnsupportedFilter`] when `workspace` is present.
    pub fn new(
        query: Option<&str>,
        states: Option<Vec<CatalogRepositoryState>>,
        workspace: Option<&str>,
    ) -> Result<Self, CatalogError> {
        if workspace.is_some() {
            return Err(CatalogError::UnsupportedFilter("workspace"));
        }
        let query = query.map(normalize_query).transpose()?.flatten();
        let states = states.map(|states| {
            let canonical: BTreeSet<_> = states.into_iter().collect();
            canonical.into_iter().collect()
        });
        Ok(Self { query, states })
    }

    /// Returns the normalized query, when nonempty.
    #[must_use]
    pub fn query(&self) -> Option<&str> {
        self.query.as_deref()
    }

    /// Returns canonical sorted state filters, or `None` for all states.
    #[must_use]
    pub fn states(&self) -> Option<&[CatalogRepositoryState]> {
        self.states.as_deref()
    }

    fn matches(&self, record: &CatalogRepositoryRecord) -> bool {
        if self
            .states
            .as_ref()
            .is_some_and(|states| states.binary_search(&record.state).is_err())
        {
            return false;
        }
        let Some(query) = &self.query else {
            return true;
        };
        canonical_text(&record.display_name).contains(query)
            || record
                .alias
                .as_deref()
                .is_some_and(|alias| canonical_text(alias).contains(query))
            || record.repository.to_string().contains(query)
    }
}

/// Checked number of repositories returned by one page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogPageSize(u16);

impl CatalogPageSize {
    /// Creates a page size from 1 through [`CATALOG_MAX_PAGE_SIZE`].
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError::InvalidPageSize`] outside the supported range.
    pub const fn new(value: u16) -> Result<Self, CatalogError> {
        if value == 0 || value > CATALOG_MAX_PAGE_SIZE {
            Err(CatalogError::InvalidPageSize)
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the checked page size.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// Stable last-sort-key for immutable catalog pagination.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CatalogSortKey {
    normalized_display_name: String,
    repository: RepositoryId,
}

impl CatalogSortKey {
    /// Creates a key from an already canonical normalized display name.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError::InvalidSortKey`] when the name is empty,
    /// oversized, or not exact NFD + default case-fold + NFC output.
    pub fn new(
        normalized_display_name: String,
        repository: RepositoryId,
    ) -> Result<Self, CatalogError> {
        if normalized_display_name.is_empty()
            || normalized_display_name.len() > CATALOG_MAX_NORMALIZED_TEXT_BYTES
            || canonical_text(&normalized_display_name) != normalized_display_name
        {
            return Err(CatalogError::InvalidSortKey);
        }
        Ok(Self {
            normalized_display_name,
            repository,
        })
    }

    /// Decodes one bounded versioned binary continuation key.
    ///
    /// Version 1 is a little-endian `u16` name byte length, canonical UTF-8
    /// name bytes, and exactly 16 repository-ID bytes.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError::UnsupportedSortVersion`] for another version,
    /// or [`CatalogError::InvalidSortKey`] for malformed or noncanonical bytes.
    pub fn from_bytes(sort_version: u16, bytes: &[u8]) -> Result<Self, CatalogError> {
        if sort_version != CATALOG_SORT_VERSION {
            return Err(CatalogError::UnsupportedSortVersion);
        }
        if bytes.len() < size_of::<u16>() + 16 || bytes.len() > CATALOG_MAX_SORT_KEY_BYTES {
            return Err(CatalogError::InvalidSortKey);
        }
        let name_length = u16::from_le_bytes(
            bytes
                .get(..2)
                .and_then(|prefix| prefix.try_into().ok())
                .ok_or(CatalogError::InvalidSortKey)?,
        );
        let name_length = usize::from(name_length);
        if name_length == 0 || name_length > CATALOG_MAX_NORMALIZED_TEXT_BYTES {
            return Err(CatalogError::InvalidSortKey);
        }
        let expected = size_of::<u16>()
            .checked_add(name_length)
            .and_then(|length| length.checked_add(16))
            .ok_or(CatalogError::InvalidSortKey)?;
        if bytes.len() != expected {
            return Err(CatalogError::InvalidSortKey);
        }
        let name_end = size_of::<u16>() + name_length;
        let normalized_display_name = std::str::from_utf8(
            bytes
                .get(size_of::<u16>()..name_end)
                .ok_or(CatalogError::InvalidSortKey)?,
        )
        .map_err(|_| CatalogError::InvalidSortKey)?
        .to_owned();
        let repository_bytes: [u8; 16] = bytes
            .get(name_end..)
            .and_then(|suffix| suffix.try_into().ok())
            .ok_or(CatalogError::InvalidSortKey)?;
        Self::new(
            normalized_display_name,
            RepositoryId::from_bytes(repository_bytes),
        )
    }

    /// Encodes this key using [`CATALOG_SORT_VERSION`].
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let name_length = u16::try_from(self.normalized_display_name.len()).unwrap_or(1_024);
        let mut bytes = Vec::with_capacity(
            size_of::<u16>()
                + self.normalized_display_name.len()
                + self.repository.as_bytes().len(),
        );
        bytes.extend_from_slice(&name_length.to_le_bytes());
        bytes.extend_from_slice(self.normalized_display_name.as_bytes());
        bytes.extend_from_slice(self.repository.as_bytes());
        bytes
    }

    /// Returns the canonical normalized display name.
    #[must_use]
    pub fn normalized_display_name(&self) -> &str {
        &self.normalized_display_name
    }

    /// Returns the repository tie-breaker.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }
}

/// One checked request for a first or continuation catalog page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogPageRequest {
    snapshot_id: Option<CatalogSnapshotId>,
    after: Option<CatalogSortKey>,
    filter: CatalogListFilter,
    page_size: CatalogPageSize,
}

impl CatalogPageRequest {
    /// Creates a checked page request.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError::SnapshotMismatch`] when `after` is present
    /// without a snapshot identity.
    pub fn new(
        snapshot_id: Option<CatalogSnapshotId>,
        after: Option<CatalogSortKey>,
        filter: CatalogListFilter,
        page_size: CatalogPageSize,
    ) -> Result<Self, CatalogError> {
        if snapshot_id.is_none() && after.is_some() {
            return Err(CatalogError::SnapshotMismatch);
        }
        Ok(Self {
            snapshot_id,
            after,
            filter,
            page_size,
        })
    }

    /// Returns the requested snapshot, or `None` for a first page.
    #[must_use]
    pub const fn snapshot_id(&self) -> Option<CatalogSnapshotId> {
        self.snapshot_id
    }

    /// Returns the exclusive last-sort-key, when continuing.
    #[must_use]
    pub fn after(&self) -> Option<&CatalogSortKey> {
        self.after.as_ref()
    }

    /// Returns the canonical filter bound to the pagination session.
    #[must_use]
    pub const fn filter(&self) -> &CatalogListFilter {
        &self.filter
    }

    /// Returns the page size bound to the pagination session.
    #[must_use]
    pub const fn page_size(&self) -> CatalogPageSize {
        self.page_size
    }
}

/// One immutable page and its continuation metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogPage {
    snapshot_id: CatalogSnapshotId,
    items: Vec<CatalogRepositoryRecord>,
    total_count: u64,
    next_after: Option<CatalogSortKey>,
}

impl CatalogPage {
    /// Returns the immutable snapshot identity.
    #[must_use]
    pub const fn snapshot_id(&self) -> CatalogSnapshotId {
        self.snapshot_id
    }

    /// Returns repositories in canonical total order.
    #[must_use]
    pub fn items(&self) -> &[CatalogRepositoryRecord] {
        &self.items
    }

    /// Consumes the page and returns its repository items.
    #[must_use]
    pub fn into_items(self) -> Vec<CatalogRepositoryRecord> {
        self.items
    }

    /// Returns the exact number of records matching the frozen filter.
    #[must_use]
    pub const fn total_count(&self) -> u64 {
        self.total_count
    }

    /// Returns the exclusive continuation key when another page exists.
    #[must_use]
    pub const fn next_after(&self) -> Option<&CatalogSortKey> {
        self.next_after.as_ref()
    }

    /// Returns the sort-key encoding version.
    #[must_use]
    pub const fn sort_version(&self) -> u16 {
        CATALOG_SORT_VERSION
    }
}

/// Stable failures from catalog snapshot construction and pagination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CatalogError {
    /// Snapshot limits are zero or internally inconsistent.
    #[error("catalog snapshot limits are invalid")]
    InvalidLimits,
    /// The public label is empty, oversized, or unsafe to expose.
    #[error("catalog repository label is invalid")]
    InvalidLabel,
    /// The query exceeds its raw or normalized hard bound.
    #[error("catalog query is invalid")]
    InvalidQuery,
    /// The language identifier is invalid.
    #[error("catalog language is invalid")]
    InvalidLanguage,
    /// Coverage counters contradict each other.
    #[error("catalog language coverage is invalid")]
    InvalidCoverage,
    /// More language entries were supplied than the record permits.
    #[error("catalog language coverage exceeds the hard bound")]
    TooManyLanguages,
    /// One language appears more than once.
    #[error("catalog language coverage contains a duplicate")]
    DuplicateLanguage,
    /// Active-generation and generation-count metadata disagree.
    #[error("catalog generation state is invalid")]
    InvalidGenerationState,
    /// The requested page size is outside the public bound.
    #[error("catalog page size is invalid")]
    InvalidPageSize,
    /// The requested filter has no authoritative implementation.
    #[error("catalog filter is unsupported: {0}")]
    UnsupportedFilter(&'static str),
    /// The sort-key encoding version is unsupported.
    #[error("catalog sort-key version is unsupported")]
    UnsupportedSortVersion,
    /// The last-sort-key is malformed, oversized, or noncanonical.
    #[error("catalog continuation key is invalid")]
    InvalidSortKey,
    /// Snapshot identity, filter, page size, or continuation key disagrees.
    #[error("catalog snapshot continuation does not match")]
    SnapshotMismatch,
    /// The requested snapshot expired.
    #[error("catalog snapshot expired")]
    SnapshotExpired,
    /// The requested snapshot was deterministically evicted.
    #[error("catalog snapshot was evicted")]
    SnapshotEvicted,
    /// The requested snapshot is not available in this process.
    #[error("catalog snapshot is unavailable")]
    SnapshotUnavailable,
    /// The first page contains more entries than one snapshot permits.
    #[error("catalog snapshot entry bound was exceeded")]
    SnapshotEntryBound,
    /// One snapshot exceeds the store's logical byte bound.
    #[error("catalog snapshot byte bound was exceeded")]
    SnapshotByteBound,
    /// The authoritative catalog yielded the same repository twice.
    #[error("catalog contains a duplicate repository")]
    DuplicateRepository,
    /// The caller-supplied monotonic timestamp moved backwards.
    #[error("catalog monotonic time moved backwards")]
    TimeRegressed,
    /// Snapshot identity or expiry arithmetic is exhausted.
    #[error("catalog snapshot identity is exhausted")]
    IdentityExhausted,
    /// Authoritative repository metadata violates a service invariant.
    #[error("catalog repository metadata is inconsistent")]
    CatalogInvariant,
}

#[derive(Debug, Clone)]
struct FrozenRecord {
    sort_key: CatalogSortKey,
    record: CatalogRepositoryRecord,
}

#[derive(Debug, Clone)]
struct Snapshot {
    filter: CatalogListFilter,
    page_size: CatalogPageSize,
    records: Vec<FrozenRecord>,
    total_count: u64,
    logical_bytes: usize,
    created_at: CatalogInstant,
    expires_at: CatalogInstant,
    sequence: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnavailableReason {
    Expired,
    Evicted,
}

/// Service-owned bounded store for immutable repository-list snapshots.
///
/// Retention bytes are logical canonical record bytes, including the stored
/// sort key, rather than allocator-dependent capacity. Deterministic expiry and
/// oldest-first eviction keep behavior independent of hash-map iteration.
pub struct CatalogSnapshotStore {
    limits: CatalogSnapshotLimits,
    instance_nonce: [u8; 32],
    snapshots: BTreeMap<CatalogSnapshotId, Snapshot>,
    creation_order: BTreeMap<(CatalogInstant, u64), CatalogSnapshotId>,
    unavailable: BTreeMap<CatalogSnapshotId, UnavailableReason>,
    unavailable_order: VecDeque<CatalogSnapshotId>,
    retained_entries: usize,
    retained_bytes: usize,
    next_sequence: u64,
    last_now: Option<CatalogInstant>,
}

impl CatalogSnapshotStore {
    /// Creates an empty store with checked hard limits and a process nonce.
    ///
    /// `instance_nonce` must be generated independently for each service
    /// process. It prevents a still-valid cursor from resolving to a different
    /// snapshot after a daemon restart resets the local sequence.
    #[must_use]
    pub fn new(limits: CatalogSnapshotLimits, instance_nonce: [u8; 32]) -> Self {
        Self {
            limits,
            instance_nonce,
            snapshots: BTreeMap::new(),
            creation_order: BTreeMap::new(),
            unavailable: BTreeMap::new(),
            unavailable_order: VecDeque::new(),
            retained_entries: 0,
            retained_bytes: 0,
            next_sequence: 0,
            last_now: None,
        }
    }

    /// Resolves a first or continuation page.
    ///
    /// `records` is evaluated only for a first page. A continuation reads only
    /// the already-frozen snapshot, so catalog mutation cannot alter membership
    /// or ordering within the session.
    ///
    /// # Errors
    ///
    /// Returns a stable [`CatalogError`] for bounds, invalid metadata,
    /// unavailable snapshots, filter or page-size mismatch, invalid
    /// continuation keys, or regressed monotonic time.
    pub fn page<I>(
        &mut self,
        request: CatalogPageRequest,
        records: I,
        now: CatalogInstant,
    ) -> Result<CatalogPage, CatalogError>
    where
        I: IntoIterator<Item = CatalogRepositoryRecord>,
    {
        self.observe_now(now)?;
        self.expire(now);
        let snapshot_id = match request.snapshot_id {
            Some(snapshot_id) => {
                self.validate_existing(snapshot_id, &request)?;
                snapshot_id
            }
            None => self.freeze(records, &request, now)?,
        };
        self.read_page(snapshot_id, request.after.as_ref())
    }

    /// Returns the number of currently retained immutable snapshots.
    #[must_use]
    pub fn retained_snapshots(&self) -> usize {
        self.snapshots.len()
    }

    /// Returns the exact logical bytes retained by live snapshots.
    #[must_use]
    pub const fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    fn observe_now(&mut self, now: CatalogInstant) -> Result<(), CatalogError> {
        if self.last_now.is_some_and(|last| now < last) {
            return Err(CatalogError::TimeRegressed);
        }
        self.last_now = Some(now);
        Ok(())
    }

    fn freeze<I>(
        &mut self,
        records: I,
        request: &CatalogPageRequest,
        now: CatalogInstant,
    ) -> Result<CatalogSnapshotId, CatalogError>
    where
        I: IntoIterator<Item = CatalogRepositoryRecord>,
    {
        let mut frozen = Vec::new();
        let mut repositories = BTreeSet::new();
        for record in records {
            if !repositories.insert(record.repository) {
                return Err(CatalogError::DuplicateRepository);
            }
            if !request.filter.matches(&record) {
                continue;
            }
            if frozen.len() == self.limits.maximum_entries_per_snapshot {
                return Err(CatalogError::SnapshotEntryBound);
            }
            let sort_key =
                CatalogSortKey::new(canonical_text(&record.display_name), record.repository)?;
            frozen.push(FrozenRecord { sort_key, record });
        }
        frozen.sort_by(|left, right| left.sort_key.cmp(&right.sort_key));
        let logical_bytes = snapshot_logical_bytes(&frozen)?;
        if logical_bytes > self.limits.maximum_retained_bytes {
            return Err(CatalogError::SnapshotByteBound);
        }
        let total_count =
            u64::try_from(frozen.len()).map_err(|_| CatalogError::SnapshotEntryBound)?;
        let expires_at = CatalogInstant(
            now.0
                .checked_add(self.limits.ttl_millis)
                .ok_or(CatalogError::IdentityExhausted)?,
        );
        let sequence = self.next_sequence;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(CatalogError::IdentityExhausted)?;
        let snapshot_id = snapshot_id(
            &self.instance_nonce,
            sequence,
            &request.filter,
            request.page_size,
            &frozen,
        );

        self.evict_until_fits(frozen.len(), logical_bytes);
        let snapshot = Snapshot {
            filter: request.filter.clone(),
            page_size: request.page_size,
            records: frozen,
            total_count,
            logical_bytes,
            created_at: now,
            expires_at,
            sequence,
        };
        self.retained_entries = self
            .retained_entries
            .checked_add(snapshot.records.len())
            .ok_or(CatalogError::SnapshotEntryBound)?;
        self.retained_bytes = self
            .retained_bytes
            .checked_add(snapshot.logical_bytes)
            .ok_or(CatalogError::SnapshotByteBound)?;
        self.creation_order.insert((now, sequence), snapshot_id);
        self.snapshots.insert(snapshot_id, snapshot);
        Ok(snapshot_id)
    }

    fn validate_existing(
        &self,
        snapshot_id: CatalogSnapshotId,
        request: &CatalogPageRequest,
    ) -> Result<(), CatalogError> {
        let Some(snapshot) = self.snapshots.get(&snapshot_id) else {
            return Err(match self.unavailable.get(&snapshot_id) {
                Some(UnavailableReason::Expired) => CatalogError::SnapshotExpired,
                Some(UnavailableReason::Evicted) => CatalogError::SnapshotEvicted,
                None => CatalogError::SnapshotUnavailable,
            });
        };
        if snapshot.filter != request.filter || snapshot.page_size != request.page_size {
            return Err(CatalogError::SnapshotMismatch);
        }
        if let Some(after) = &request.after
            && snapshot
                .records
                .binary_search_by(|record| record.sort_key.cmp(after))
                .is_err()
        {
            return Err(CatalogError::SnapshotMismatch);
        }
        Ok(())
    }

    fn read_page(
        &self,
        snapshot_id: CatalogSnapshotId,
        after: Option<&CatalogSortKey>,
    ) -> Result<CatalogPage, CatalogError> {
        let snapshot = self
            .snapshots
            .get(&snapshot_id)
            .ok_or(CatalogError::SnapshotUnavailable)?;
        let start = match after {
            Some(after) => snapshot
                .records
                .binary_search_by(|record| record.sort_key.cmp(after))
                .map_err(|_| CatalogError::SnapshotMismatch)?
                .checked_add(1)
                .ok_or(CatalogError::SnapshotMismatch)?,
            None => 0,
        };
        let end = start
            .saturating_add(usize::from(snapshot.page_size.get()))
            .min(snapshot.records.len());
        let items = snapshot
            .records
            .get(start..end)
            .ok_or(CatalogError::SnapshotMismatch)?
            .iter()
            .map(|record| record.record.clone())
            .collect();
        let next_after = if end < snapshot.records.len() {
            snapshot
                .records
                .get(end.saturating_sub(1))
                .map(|record| record.sort_key.clone())
        } else {
            None
        };
        Ok(CatalogPage {
            snapshot_id,
            items,
            total_count: snapshot.total_count,
            next_after,
        })
    }

    fn expire(&mut self, now: CatalogInstant) {
        let expired: Vec<_> = self
            .snapshots
            .iter()
            .filter_map(|(id, snapshot)| (now >= snapshot.expires_at).then_some(*id))
            .collect();
        for id in expired {
            self.remove_snapshot(id, UnavailableReason::Expired);
        }
    }

    fn evict_until_fits(&mut self, incoming_entries: usize, incoming_bytes: usize) {
        while self.snapshots.len() >= self.limits.maximum_snapshots
            || self.retained_entries.saturating_add(incoming_entries)
                > self.limits.maximum_retained_entries
            || self.retained_bytes.saturating_add(incoming_bytes)
                > self.limits.maximum_retained_bytes
        {
            let Some((_, oldest)) = self.creation_order.first_key_value() else {
                break;
            };
            let oldest = *oldest;
            self.remove_snapshot(oldest, UnavailableReason::Evicted);
        }
    }

    fn remove_snapshot(&mut self, id: CatalogSnapshotId, reason: UnavailableReason) {
        let Some(snapshot) = self.snapshots.remove(&id) else {
            return;
        };
        self.creation_order
            .remove(&(snapshot.created_at, snapshot.sequence));
        self.retained_entries = self.retained_entries.saturating_sub(snapshot.records.len());
        self.retained_bytes = self.retained_bytes.saturating_sub(snapshot.logical_bytes);
        self.record_unavailable(id, reason);
    }

    fn record_unavailable(&mut self, id: CatalogSnapshotId, reason: UnavailableReason) {
        self.unavailable.insert(id, reason);
        self.unavailable_order.push_back(id);
        let maximum_tombstones = self
            .limits
            .maximum_snapshots
            .saturating_mul(MAX_TOMBSTONES_MULTIPLIER);
        while self.unavailable_order.len() > maximum_tombstones {
            if let Some(oldest) = self.unavailable_order.pop_front() {
                self.unavailable.remove(&oldest);
            }
        }
    }
}

fn normalize_query(query: &str) -> Result<Option<String>, CatalogError> {
    if query.chars().count() > MAX_QUERY_CHARACTERS {
        return Err(CatalogError::InvalidQuery);
    }
    let normalized = canonical_text(query);
    if normalized.len() > CATALOG_MAX_NORMALIZED_TEXT_BYTES {
        return Err(CatalogError::InvalidQuery);
    }
    Ok((!normalized.is_empty()).then_some(normalized))
}

fn canonical_text(value: &str) -> String {
    value.nfd().case_fold().nfc().collect()
}

pub(super) fn validate_label(label: &str) -> Result<(), CatalogError> {
    if label.is_empty()
        || label.len() > CATALOG_MAX_LABEL_BYTES
        || label
            .chars()
            .any(|character| character.is_control() || matches!(character, '/' | '\\'))
    {
        Err(CatalogError::InvalidLabel)
    } else {
        Ok(())
    }
}

fn snapshot_logical_bytes(records: &[FrozenRecord]) -> Result<usize, CatalogError> {
    records.iter().try_fold(0usize, |total, record| {
        total
            .checked_add(record_logical_bytes(record))
            .ok_or(CatalogError::SnapshotByteBound)
    })
}

fn record_logical_bytes(record: &FrozenRecord) -> usize {
    let fixed = 16usize
        .saturating_add(20)
        .saturating_add(size_of::<u64>())
        .saturating_add(3)
        .saturating_add(size_of::<u16>());
    let labels = record
        .record
        .display_name
        .len()
        .saturating_add(record.record.alias.as_ref().map_or(0, String::len))
        .saturating_add(record.record.root_path.as_ref().map_or(0, String::len))
        .saturating_add(record.sort_key.normalized_display_name.len());
    record
        .record
        .coverage
        .iter()
        .fold(fixed.saturating_add(labels), |bytes, coverage| {
            bytes
                .saturating_add(coverage.language.len())
                .saturating_add(2)
                .saturating_add(2 * size_of::<u64>())
        })
}

fn snapshot_id(
    instance_nonce: &[u8; 32],
    sequence: u64,
    filter: &CatalogListFilter,
    page_size: CatalogPageSize,
    records: &[FrozenRecord],
) -> CatalogSnapshotId {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(SNAPSHOT_ID_CONTEXT);
    bytes.extend_from_slice(instance_nonce);
    bytes.extend_from_slice(&sequence.to_le_bytes());
    bytes.extend_from_slice(&page_size.get().to_le_bytes());
    match filter.query() {
        Some(query) => {
            bytes.push(1);
            append_length_prefixed(&mut bytes, query.as_bytes());
        }
        None => bytes.push(0),
    }
    match filter.states() {
        Some(states) => {
            bytes.push(1);
            bytes.extend_from_slice(
                &u64::try_from(states.len())
                    .unwrap_or(u64::MAX)
                    .to_le_bytes(),
            );
            for state in states {
                append_length_prefixed(&mut bytes, state.as_str().as_bytes());
            }
        }
        None => bytes.push(0),
    }
    bytes.extend_from_slice(
        &u64::try_from(records.len())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    for frozen in records {
        append_length_prefixed(
            &mut bytes,
            frozen.sort_key.normalized_display_name.as_bytes(),
        );
        bytes.extend_from_slice(frozen.record.repository.as_bytes());
        match frozen.record.active_generation {
            Some(generation) => {
                bytes.push(1);
                bytes.extend_from_slice(generation.as_bytes());
            }
            None => bytes.push(0),
        }
        bytes.extend_from_slice(&frozen.record.generation_count.to_le_bytes());
        append_length_prefixed(&mut bytes, frozen.record.state.as_str().as_bytes());
    }
    CatalogSnapshotId(content_hash(&bytes))
}

fn append_length_prefixed(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
    output.extend_from_slice(value);
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    const NOW: CatalogInstant = CatalogInstant::from_millis(1_000);

    fn repository(index: u128) -> RepositoryId {
        RepositoryId::from_bytes(index.to_be_bytes())
    }

    fn generation(index: u128) -> GenerationId {
        let mut bytes = [0; 20];
        bytes[4..].copy_from_slice(&index.to_be_bytes());
        GenerationId::from_bytes(bytes)
    }

    fn record(index: u128, name: &str) -> CatalogRepositoryRecord {
        CatalogRepositoryRecord::new(
            repository(index),
            name.to_owned(),
            Some(generation(index)),
            1,
            CatalogRepositoryState::Ready,
        )
        .expect("fixture record is valid")
        .with_coverage(vec![
            CatalogLanguageCoverage::new(
                "rust".to_owned(),
                AnalysisTier::TierA,
                CoverageStatus::Complete,
                1,
                1,
            )
            .expect("fixture coverage is valid"),
        ])
        .expect("fixture language is unique")
    }

    fn filter() -> CatalogListFilter {
        CatalogListFilter::new(None, None, None).expect("empty filter is valid")
    }

    fn store() -> CatalogSnapshotStore {
        store_with_limits(CatalogSnapshotLimits::default())
    }

    fn store_with_limits(limits: CatalogSnapshotLimits) -> CatalogSnapshotStore {
        CatalogSnapshotStore::new(limits, [0x5a; 32])
    }

    fn request(
        snapshot_id: Option<CatalogSnapshotId>,
        after: Option<CatalogSortKey>,
        page_size: u16,
    ) -> CatalogPageRequest {
        CatalogPageRequest::new(
            snapshot_id,
            after,
            filter(),
            CatalogPageSize::new(page_size).expect("fixture page size is valid"),
        )
        .expect("fixture request is valid")
    }

    fn collect_all(
        store: &mut CatalogSnapshotStore,
        records: Vec<CatalogRepositoryRecord>,
        page_size: u16,
    ) -> Vec<CatalogRepositoryRecord> {
        let first = store
            .page(request(None, None, page_size), records, NOW)
            .expect("first page succeeds");
        let snapshot_id = first.snapshot_id();
        let mut after = first.next_after().cloned();
        let mut output = first.into_items();
        while let Some(key) = after {
            let page = store
                .page(
                    request(Some(snapshot_id), Some(key), page_size),
                    Vec::new(),
                    NOW,
                )
                .expect("continuation succeeds");
            after = page.next_after().cloned();
            output.extend(page.into_items());
        }
        output
    }

    #[test]
    fn empty_catalog_returns_success_without_continuation() {
        let mut store = store();
        let page = store
            .page(request(None, None, 20), Vec::new(), NOW)
            .expect("empty catalog is a successful snapshot");

        assert!(page.items().is_empty());
        assert_eq!(page.total_count(), 0);
        assert!(page.next_after().is_none());
    }

    #[test]
    fn repeated_reads_of_one_snapshot_are_identical() {
        let mut store = store();
        let first = store
            .page(
                request(None, None, 2),
                vec![record(1, "alpha"), record(2, "bravo"), record(3, "charlie")],
                NOW,
            )
            .expect("first page succeeds");
        let repeated = store
            .page(request(Some(first.snapshot_id()), None, 2), Vec::new(), NOW)
            .expect("pinned first page repeats");

        assert_eq!(repeated, first);
    }

    #[test]
    fn process_nonce_prevents_snapshot_identity_reuse_after_restart() {
        let mut first_store = CatalogSnapshotStore::new(CatalogSnapshotLimits::default(), [1; 32]);
        let mut restarted_store =
            CatalogSnapshotStore::new(CatalogSnapshotLimits::default(), [2; 32]);
        let records = vec![record(1, "alpha")];
        let first = first_store
            .page(request(None, None, 20), records.clone(), NOW)
            .expect("first process snapshot succeeds");
        let restarted = restarted_store
            .page(request(None, None, 20), records, NOW)
            .expect("restarted process snapshot succeeds");

        assert_ne!(first.snapshot_id(), restarted.snapshot_id());
    }

    #[test]
    fn one_exact_boundary_and_final_pages_are_precise() {
        let mut store = store();
        let one = store
            .page(request(None, None, 1), vec![record(1, "one")], NOW)
            .expect("one-item page succeeds");
        assert_eq!(one.items().len(), 1);
        assert!(one.next_after().is_none());

        let exact = store
            .page(
                request(None, None, 2),
                vec![record(1, "one"), record(2, "two")],
                NOW,
            )
            .expect("exact-boundary page succeeds");
        assert_eq!(exact.items().len(), 2);
        assert!(exact.next_after().is_none());

        let first = store
            .page(
                request(None, None, 2),
                vec![record(1, "one"), record(2, "two"), record(3, "three")],
                NOW,
            )
            .expect("multi-page snapshot succeeds");
        let final_page = store
            .page(
                request(Some(first.snapshot_id()), first.next_after().cloned(), 2),
                Vec::new(),
                NOW,
            )
            .expect("final page succeeds");
        assert_eq!(final_page.items().len(), 1);
        assert!(final_page.next_after().is_none());
    }

    #[test]
    fn insertion_deletion_rename_and_reorder_do_not_change_snapshot() {
        let baseline = vec![
            record(1, "alpha"),
            record(2, "bravo"),
            record(3, "charlie"),
            record(4, "delta"),
        ];
        for mutated in [
            vec![
                record(1, "alpha"),
                record(2, "bravo"),
                record(3, "charlie"),
                record(4, "delta"),
                record(5, "aardvark"),
            ],
            vec![record(1, "alpha"), record(3, "charlie"), record(4, "delta")],
            vec![
                record(1, "zulu"),
                record(2, "bravo"),
                record(3, "charlie"),
                record(4, "delta"),
            ],
            vec![
                record(4, "delta"),
                record(2, "bravo"),
                record(1, "alpha"),
                record(3, "charlie"),
            ],
        ] {
            let mut store = store();
            let first = store
                .page(request(None, None, 2), baseline.clone(), NOW)
                .expect("first page succeeds");
            let second = store
                .page(
                    request(Some(first.snapshot_id()), first.next_after().cloned(), 2),
                    mutated,
                    NOW,
                )
                .expect("continuation ignores mutable catalog input");
            let ids: Vec<_> = first
                .items()
                .iter()
                .chain(second.items())
                .map(CatalogRepositoryRecord::repository)
                .collect();
            assert_eq!(
                ids,
                vec![repository(1), repository(2), repository(3), repository(4)]
            );
        }
    }

    #[test]
    fn normalized_name_then_repository_is_a_total_order() {
        let mut store = store();
        let records = collect_all(
            &mut store,
            vec![
                record(3, "Zulu"),
                record(2, "éclair"),
                record(1, "e\u{301}clair"),
                record(4, "alpha"),
            ],
            2,
        );
        let ids: Vec<_> = records
            .iter()
            .map(CatalogRepositoryRecord::repository)
            .collect();
        assert_eq!(
            ids,
            vec![repository(4), repository(3), repository(1), repository(2)]
        );
    }

    #[test]
    fn query_normalization_matches_mcp_unicode_semantics() {
        let sharp_s =
            CatalogListFilter::new(Some("Straße"), None, None).expect("Unicode query is valid");
        let expanded =
            CatalogListFilter::new(Some("STRASSE"), None, None).expect("Unicode query is valid");
        let decomposed =
            CatalogListFilter::new(Some("e\u{301}"), None, None).expect("Unicode query is valid");
        let composed =
            CatalogListFilter::new(Some("é"), None, None).expect("Unicode query is valid");

        assert_eq!(sharp_s, expanded);
        assert_eq!(decomposed, composed);
        assert_eq!(sharp_s.query(), Some("strasse"));
        assert_eq!(
            CatalogListFilter::new(Some(""), None, None)
                .expect("empty query is valid")
                .query(),
            None
        );
    }

    #[test]
    fn query_and_canonical_state_filters_affect_membership() {
        let ready = record(1, "Straße");
        let degraded = CatalogRepositoryRecord::new(
            repository(2),
            "other".to_owned(),
            Some(generation(2)),
            2,
            CatalogRepositoryState::Degraded,
        )
        .expect("degraded fixture is valid")
        .with_alias(Some("STRASSE mirror".to_owned()))
        .expect("alias is valid");
        let canonical_filter = CatalogListFilter::new(
            Some("strasse"),
            Some(vec![
                CatalogRepositoryState::Degraded,
                CatalogRepositoryState::Ready,
                CatalogRepositoryState::Degraded,
            ]),
            None,
        )
        .expect("filter is valid");
        assert_eq!(
            canonical_filter.states(),
            Some(
                [
                    CatalogRepositoryState::Ready,
                    CatalogRepositoryState::Degraded
                ]
                .as_slice()
            )
        );

        let mut store = store();
        let page = store
            .page(
                CatalogPageRequest::new(
                    None,
                    None,
                    canonical_filter,
                    CatalogPageSize::new(20).expect("page size is valid"),
                )
                .expect("request is valid"),
                vec![ready, degraded],
                NOW,
            )
            .expect("filtered page succeeds");
        assert_eq!(page.total_count(), 2);

        let no_states = CatalogListFilter::new(Some("strasse"), Some(Vec::new()), None)
            .expect("empty state set is valid");
        let empty = store
            .page(
                CatalogPageRequest::new(
                    None,
                    None,
                    no_states,
                    CatalogPageSize::new(20).expect("page size is valid"),
                )
                .expect("request is valid"),
                vec![record(1, "Straße")],
                NOW,
            )
            .expect("empty state set yields an empty page");
        assert!(empty.items().is_empty());
    }

    #[test]
    fn unsupported_workspace_filter_fails_closed() {
        assert_eq!(
            CatalogListFilter::new(None, None, Some("workspace")),
            Err(CatalogError::UnsupportedFilter("workspace"))
        );
    }

    #[test]
    fn sort_key_binary_round_trip_is_exact_and_bounded() {
        let key = CatalogSortKey::new("éclair".to_owned(), repository(7))
            .expect("canonical key is valid");
        let bytes = key.to_bytes();
        assert!(bytes.len() <= CATALOG_MAX_SORT_KEY_BYTES);
        assert_eq!(
            CatalogSortKey::from_bytes(CATALOG_SORT_VERSION, &bytes).expect("key decodes"),
            key
        );
        assert_eq!(
            CatalogSortKey::from_bytes(CATALOG_SORT_VERSION + 1, &bytes),
            Err(CatalogError::UnsupportedSortVersion)
        );

        let mut trailing = bytes;
        trailing.push(0);
        assert_eq!(
            CatalogSortKey::from_bytes(CATALOG_SORT_VERSION, &trailing),
            Err(CatalogError::InvalidSortKey)
        );
    }

    #[test]
    fn expiry_eviction_unknown_and_mismatch_are_distinct() {
        let ttl = CATALOG_CURSOR_TTL_MILLIS + 1;
        let limits = CatalogSnapshotLimits::new(1, 4, 4, 4_096, ttl).expect("limits are valid");
        let mut store = store_with_limits(limits);
        let first = store
            .page(request(None, None, 1), vec![record(1, "one")], NOW)
            .expect("first snapshot succeeds");
        let first_id = first.snapshot_id();
        let second = store
            .page(
                request(None, None, 1),
                vec![record(2, "two")],
                CatalogInstant::from_millis(1_001),
            )
            .expect("second snapshot evicts the first");
        assert_eq!(
            store.page(
                request(Some(first_id), None, 1),
                Vec::new(),
                CatalogInstant::from_millis(1_001),
            ),
            Err(CatalogError::SnapshotEvicted)
        );

        let wrong_filter =
            CatalogListFilter::new(Some("two"), None, None).expect("filter is valid");
        assert_eq!(
            store.page(
                CatalogPageRequest::new(
                    Some(second.snapshot_id()),
                    None,
                    wrong_filter,
                    CatalogPageSize::new(1).expect("page size is valid"),
                )
                .expect("request is structurally valid"),
                Vec::new(),
                CatalogInstant::from_millis(1_001),
            ),
            Err(CatalogError::SnapshotMismatch)
        );
        assert_eq!(
            store.page(
                request(Some(CatalogSnapshotId::from_bytes([9; 32])), None, 1),
                Vec::new(),
                CatalogInstant::from_millis(1_001),
            ),
            Err(CatalogError::SnapshotUnavailable)
        );
        assert_eq!(
            store.page(
                request(Some(second.snapshot_id()), None, 1),
                Vec::new(),
                CatalogInstant::from_millis(1_001 + ttl),
            ),
            Err(CatalogError::SnapshotExpired)
        );
    }

    #[test]
    fn default_snapshot_outlives_cursor_window_and_expires_at_ten_minutes() {
        let mut store = store();
        let first = store
            .page(request(None, None, 1), vec![record(1, "one")], NOW)
            .expect("first snapshot succeeds");
        let snapshot_id = first.snapshot_id();

        store
            .page(
                request(Some(snapshot_id), None, 1),
                Vec::new(),
                CatalogInstant::from_millis(NOW.as_millis() + 5 * 60 * 1_000),
            )
            .expect("snapshot remains available at the cursor lifetime");
        assert_eq!(
            store.page(
                request(Some(snapshot_id), None, 1),
                Vec::new(),
                CatalogInstant::from_millis(NOW.as_millis() + CATALOG_SNAPSHOT_TTL_MILLIS),
            ),
            Err(CatalogError::SnapshotExpired)
        );
    }

    #[test]
    fn continuation_rejects_wrong_key_and_page_size() {
        let mut store = store();
        let first = store
            .page(
                request(None, None, 1),
                vec![record(1, "one"), record(2, "two")],
                NOW,
            )
            .expect("first page succeeds");
        let wrong_key =
            CatalogSortKey::new("absent".to_owned(), repository(9)).expect("key is valid");
        assert_eq!(
            store.page(
                request(Some(first.snapshot_id()), Some(wrong_key), 1),
                Vec::new(),
                NOW,
            ),
            Err(CatalogError::SnapshotMismatch)
        );
        assert_eq!(
            store.page(
                request(Some(first.snapshot_id()), first.next_after().cloned(), 2),
                Vec::new(),
                NOW,
            ),
            Err(CatalogError::SnapshotMismatch)
        );
    }

    #[test]
    fn entry_and_byte_bounds_fail_before_retention() {
        let entry_limits = CatalogSnapshotLimits::new(2, 1, 2, 4_096, CATALOG_SNAPSHOT_TTL_MILLIS)
            .expect("limits are valid");
        let mut entry_store = store_with_limits(entry_limits);
        assert_eq!(
            entry_store.page(
                request(None, None, 1),
                vec![record(1, "one"), record(2, "two")],
                NOW,
            ),
            Err(CatalogError::SnapshotEntryBound)
        );
        assert_eq!(entry_store.retained_snapshots(), 0);

        let byte_limits = CatalogSnapshotLimits::new(2, 2, 4, 8, CATALOG_SNAPSHOT_TTL_MILLIS)
            .expect("limits are valid");
        let mut byte_store = store_with_limits(byte_limits);
        assert_eq!(
            byte_store.page(request(None, None, 1), vec![record(1, "one")], NOW),
            Err(CatalogError::SnapshotByteBound)
        );
        assert_eq!(byte_store.retained_bytes(), 0);
    }

    #[test]
    fn custom_limits_cannot_exceed_service_ceilings_or_cursor_safety() {
        assert_eq!(
            CatalogSnapshotLimits::new(
                CATALOG_MAX_SNAPSHOTS + 1,
                1,
                1,
                1,
                CATALOG_SNAPSHOT_TTL_MILLIS,
            ),
            Err(CatalogError::InvalidLimits)
        );
        assert_eq!(
            CatalogSnapshotLimits::new(1, 1, 1, 1, CATALOG_CURSOR_TTL_MILLIS,),
            Err(CatalogError::InvalidLimits)
        );
    }

    #[test]
    fn labels_coverage_and_generation_invariants_fail_closed() {
        assert_eq!(
            CatalogRepositoryRecord::new(
                repository(1),
                "bad/name".to_owned(),
                Some(generation(1)),
                1,
                CatalogRepositoryState::Ready,
            ),
            Err(CatalogError::InvalidLabel)
        );
        assert_eq!(
            CatalogRepositoryRecord::new(
                repository(1),
                "valid".to_owned(),
                Some(generation(1)),
                0,
                CatalogRepositoryState::Ready,
            ),
            Err(CatalogError::InvalidGenerationState)
        );
        assert_eq!(
            CatalogLanguageCoverage::new(
                "Rust".to_owned(),
                AnalysisTier::TierA,
                CoverageStatus::Complete,
                1,
                1,
            ),
            Err(CatalogError::InvalidLanguage)
        );
        assert_eq!(
            CatalogLanguageCoverage::new(
                "rust".to_owned(),
                AnalysisTier::TierA,
                CoverageStatus::Complete,
                1,
                2,
            ),
            Err(CatalogError::InvalidCoverage)
        );
    }

    #[test]
    fn time_regression_is_rejected_without_mutation() {
        let mut store = store();
        store
            .page(request(None, None, 1), vec![record(1, "one")], NOW)
            .expect("first page succeeds");
        assert_eq!(
            store.page(
                request(None, None, 1),
                vec![record(2, "two")],
                CatalogInstant::from_millis(NOW.as_millis() - 1),
            ),
            Err(CatalogError::TimeRegressed)
        );
        assert_eq!(store.retained_snapshots(), 1);
    }

    proptest! {
        #[test]
        fn arbitrary_catalogs_have_stable_order_without_duplicates_or_omissions(
            names in prop::collection::vec("[A-Za-z0-9é]{1,24}", 0..80),
            page_size in 1u16..=20,
        ) {
            let records: Vec<_> = names
                .iter()
                .enumerate()
                .map(|(index, name)| {
                    record(
                        u128::try_from(index + 1).expect("test index fits"),
                        name,
                    )
                })
                .collect();
            let mut expected = records.clone();
            expected.sort_by_key(|record| {
                (canonical_text(record.display_name()), record.repository())
            });
            let mut store = store();
            let actual = collect_all(&mut store, records, page_size);

            prop_assert_eq!(&actual, &expected);
            let unique: BTreeSet<_> = actual
                .iter()
                .map(CatalogRepositoryRecord::repository)
                .collect();
            prop_assert_eq!(unique.len(), actual.len());
        }

        #[test]
        fn sort_key_round_trips_canonical_names(
            name in "[A-Za-z0-9é]{1,128}",
            id in any::<u128>(),
        ) {
            let canonical = canonical_text(&name);
            let key = CatalogSortKey::new(canonical, repository(id))
                .expect("generated canonical key is bounded");
            let decoded = CatalogSortKey::from_bytes(
                CATALOG_SORT_VERSION,
                &key.to_bytes(),
            )
            .expect("encoded key decodes");
            prop_assert_eq!(decoded, key);
        }
    }
}
