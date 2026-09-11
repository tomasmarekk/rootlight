use std::time::Duration;

use rootlight_cancel::CancellationReason;
use rootlight_ids::{FileId, GenerationId, SymbolId};

/// Maximum UTF-8 bytes accepted for one indexed term.
pub(crate) const MAX_TERM_BYTES: usize = 240;
/// Tokenizer name persisted in the Tantivy schema.
pub(crate) const CODE_TOKENIZER: &str = "rootlight_code_v2";

/// Source-free accounting from one complete lexical projection scan.
///
/// Counts describe original word occurrences, not distinct postings. Delimiters
/// are not words; deduplication is not an omission. This is not semantic coverage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceLexicalCoverage {
    /// Exact input byte length, including delimiters.
    pub source_bytes: u64,
    /// Original bytes in words excluded by the term-size bound.
    pub omitted_word_bytes: u64,
    /// Number of excluded word occurrences, including repetitions.
    pub omitted_words: u64,
    /// Whether the entire input could be decoded as UTF-8.
    pub valid_utf8: bool,
}

impl SourceLexicalCoverage {
    /// Whether every source word was admitted to the lexical vocabulary.
    #[must_use]
    pub const fn is_complete(self) -> bool {
        self.valid_utf8 && self.omitted_words == 0
    }
}

/// Bounded source vocabulary and accounting produced in the same scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceTermProjection {
    /// Whole-word batches sharing one file identity.
    pub chunks: Vec<Vec<String>>,
    /// Exact source-free omission accounting.
    pub coverage: SourceLexicalCoverage,
}

/// A bounded lexical document for one semantic symbol or retained source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexicalDocument {
    /// Stable semantic symbol identity, absent for a file-only source projection.
    pub symbol_id: Option<SymbolId>,
    /// Stable identity of the declaring file.
    pub file_id: FileId,
    /// Human-readable identifier used for search and display.
    pub identifier: String,
    /// Original symbol name when it differs from the display identifier.
    /// File-only documents cannot carry this source-backed symbol alias.
    pub canonical_name: Option<String>,
    /// Qualified source spelling, including containers.
    pub qualified_name: String,
    /// Repository-relative canonical display path.
    pub path: String,
    /// Closed semantic kind label.
    pub kind: String,
    /// Canonical language identifier.
    pub language: String,
    /// Truthfulness tier assigned by the adapter.
    pub tier: String,
    /// Optional package or module ownership label.
    pub package: Option<String>,
    /// Optional build-target discriminator.
    pub build_target: Option<String>,
    /// Optional bounded signature text.
    pub signature: Option<String>,
    /// Bounded type names referenced by the signature.
    pub type_names: Vec<String>,
    /// Optional bounded untrusted documentation or comment text.
    pub documentation: Option<String>,
    /// Bounded identifiers extracted from a retained source file.
    pub source_identifiers: Vec<String>,
    /// Optional bounded retained source text.
    pub source_text: Option<String>,
    /// Bounded source-word batches indexed under the same file identity.
    ///
    /// These extend the legacy prefix fields without creating extra search
    /// hits. Each batch retains whole words so token boundaries are not cut.
    pub source_term_chunks: Vec<Vec<String>>,
    /// Whole-input accounting, absent for legacy or symbol projections.
    pub source_coverage: Option<SourceLexicalCoverage>,
    /// Whether the declaring file is generated.
    pub generated: bool,
    /// Whether the entity is test-only or test-related.
    pub test: bool,
    /// Whether the entity belongs to a declaration-only type surface.
    pub declaration_only: bool,
}

/// Resource limits for deterministic index construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildBudget {
    /// Maximum documents in one generation.
    pub max_documents: usize,
    /// Maximum aggregate UTF-8 bytes across accepted fields.
    pub max_text_bytes: usize,
    /// Maximum heap reserved by the backend indexer.
    pub indexer_memory_bytes: usize,
}

impl Default for BuildBudget {
    fn default() -> Self {
        Self {
            max_documents: 250_000,
            max_text_bytes: 512 * 1024 * 1024,
            indexer_memory_bytes: 32 * 1024 * 1024,
        }
    }
}

/// Observable result of a completed lexical build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildStats {
    /// Generation encoded into the committed index metadata.
    pub generation: GenerationId,
    /// Number of committed symbol documents.
    pub documents: u64,
    /// Aggregate validated text bytes supplied by the caller.
    pub text_bytes: u64,
}

/// Supported bounded query semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SearchMode {
    /// Case-insensitive whole-identifier match.
    Exact,
    /// Case-insensitive identifier prefix match.
    Prefix,
    /// Language-aware identifier, path, signature, and documentation search.
    Text,
    /// Restricted regular-expression syntax compiled to a finite automaton.
    SafeRegex,
    /// Restricted glob syntax compiled to a finite automaton.
    Glob,
}

/// One generation-pinned lexical query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRequest {
    /// User-supplied query text.
    pub query: String,
    /// Matching semantics.
    pub mode: SearchMode,
    /// Maximum hits returned after deterministic ordering.
    pub max_results: usize,
    /// Number of deterministically ordered hits preceding this page.
    pub page_offset: usize,
}

/// Resource limits for one query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchBudget {
    /// Maximum query length in UTF-8 bytes.
    pub max_query_bytes: usize,
    /// Maximum matched documents materialized for deterministic tie-breaking.
    pub max_candidates: usize,
    /// Maximum result count a request may ask for.
    pub max_results: usize,
    /// Maximum code-aware terms one text query may contain.
    pub max_terms: usize,
    /// Maximum distinct index terms admitted to one executable query.
    pub max_expanded_terms: usize,
    /// Maximum index terms examined while evaluating bounded pattern ranges.
    pub max_examined_terms: usize,
    /// Maximum aggregate lexical posting entries admitted before execution.
    ///
    /// Independently capped filter domains are intersected with this admitted
    /// lexical work and do not contribute their global document frequency.
    pub max_postings: u64,
    /// Maximum aggregate UTF-8 bytes materialized while selecting returned hits.
    pub max_returned_text_bytes: usize,
    /// Maximum monotonic wall time spent by one synchronous query.
    pub max_duration: Duration,
}

impl Default for SearchBudget {
    fn default() -> Self {
        Self {
            max_query_bytes: 512,
            max_candidates: 10_000,
            max_results: 100,
            max_terms: 16,
            max_expanded_terms: 1_024,
            max_examined_terms: 8_192,
            max_postings: 100_000,
            max_returned_text_bytes: 4 * 1024 * 1024,
            max_duration: Duration::from_secs(2),
        }
    }
}

/// Stable metadata returned for a lexical hit.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    /// Stable semantic symbol identity, absent for a file-only source hit.
    pub symbol_id: Option<SymbolId>,
    /// Stable identity of the declaring file.
    pub file_id: FileId,
    /// Human-readable identifier used for search and display.
    pub identifier: String,
    /// Original symbol-name alias, verified against durable IR by consumers.
    pub canonical_name: Option<String>,
    /// Qualified source spelling.
    pub qualified_name: String,
    /// Repository-relative canonical display path.
    pub path: String,
    /// Closed semantic kind label.
    pub kind: String,
    /// Canonical language identifier.
    pub language: String,
    /// Truthfulness tier assigned by the adapter.
    pub tier: String,
    /// Whether the declaring file is generated.
    pub generated: bool,
    /// Whether the entity is test-only or test-related.
    pub test: bool,
    /// Whether the entity belongs to a declaration-only type surface.
    pub declaration_only: bool,
    /// Versioned backend relevance score before stable identity tie-breaking.
    pub relevance_score: f32,
}

/// Bounded lexical result with exact backend work counters.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchOutcome {
    /// Deterministically ordered hits after the public result cap.
    pub hits: Vec<SearchHit>,
    /// Matching documents materialized before result truncation.
    pub matched_candidates: u64,
    /// Aggregate UTF-8 bytes decoded across all matching candidates.
    pub materialized_text_bytes: u64,
}

/// Document field rejected at the indexing boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DocumentField {
    /// Declared identifier.
    Identifier,
    /// Qualified identifier.
    QualifiedName,
    /// Canonical display path.
    Path,
    /// Semantic kind.
    Kind,
    /// Language identifier.
    Language,
    /// Truthfulness tier.
    Tier,
    /// Package label.
    Package,
    /// Build-target label.
    BuildTarget,
    /// Signature text.
    Signature,
    /// Referenced type name.
    TypeName,
    /// Documentation text.
    Documentation,
    /// Identifier extracted from retained source text.
    SourceIdentifier,
    /// Retained source text.
    SourceText,
}

/// Stable reason a query was rejected before reaching Tantivy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum QueryViolation {
    /// The query is empty or whitespace-only.
    Empty,
    /// The query exceeds its byte budget.
    TooLong,
    /// The query contains a term the lexical backend cannot encode safely.
    TermTooLong,
    /// The query contains an unsupported control character.
    UnsupportedCharacter,
    /// Restricted pattern syntax contains an unsupported construct.
    UnsupportedPattern,
    /// A pattern contains too many wildcard transitions.
    TooManyWildcards,
    /// A pattern has too little literal selectivity.
    InsufficientLiteral,
    /// A pattern begins with an unbounded wildcard.
    LeadingWildcard,
}

/// Borrowed unions intersected before lexical candidate accounting and paging.
///
/// Empty language/path unions are unrestricted. Kind labels use the index's raw
/// vocabulary, not presentation-layer groups: `None` is unrestricted, whereas
/// `Some(&[])` matches nothing. Labels must be sorted, unique, lowercase ASCII
/// letters/underscores, at most 128 bytes each, with at most 64 kind labels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SearchFilters<'a> {
    /// Canonical language union, using the existing language-filter contract.
    pub languages: &'a [String],
    /// Canonical repository-relative path-prefix union.
    pub path_prefixes: &'a [String],
    /// Optional exact raw entity-kind union; unknown labels match no documents.
    pub kinds: Option<&'a [String]>,
}

/// Closed, source-redacted lexical failure taxonomy.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SearchError {
    /// Cooperative cancellation or deadline expiry.
    #[error("lexical operation was cancelled: {0:?}")]
    Cancelled(CancellationReason),
    /// A document field violated a stable boundary rule.
    #[error("lexical document field is invalid: {field:?}")]
    InvalidDocument {
        /// Rejected field.
        field: DocumentField,
    },
    /// Two documents used the same semantic identity.
    #[error("lexical documents contain a duplicate symbol identity")]
    DuplicateSymbol,
    /// A construction budget was exceeded.
    #[error("lexical build budget exceeded: {resource}")]
    BuildBudgetExceeded {
        /// Stable resource label.
        resource: &'static str,
    },
    /// An artifact verification budget was exceeded.
    #[error("lexical artifact budget exceeded: {resource}")]
    ArtifactBudgetExceeded {
        /// Stable resource label.
        resource: &'static str,
    },
    /// A caller-supplied artifact budget exceeded the implementation ceiling.
    #[error("lexical artifact budget is invalid: {resource}")]
    InvalidArtifactBudget {
        /// Stable resource label.
        resource: &'static str,
    },
    /// Query syntax or bounds were invalid.
    #[error("lexical query is invalid: {0:?}")]
    InvalidQuery(QueryViolation),
    /// A language filter was empty, noncanonical, duplicated, or oversized.
    #[error("lexical language filter is invalid")]
    InvalidLanguageFilter,
    /// A repository-relative path filter was empty, noncanonical, or oversized.
    #[error("lexical path filter is invalid")]
    InvalidPathFilter,
    /// A kind union was noncanonical, oversized, or unsupported by the backend.
    #[error("lexical kind filter is invalid or unsupported")]
    InvalidKindFilter,
    /// More matches existed than the deterministic materialization budget.
    #[error("lexical candidate budget exceeded")]
    CandidateBudgetExceeded,
    /// Pattern or prefix expansion exceeded the admitted term count.
    #[error("lexical term expansion budget exceeded")]
    TermExpansionBudgetExceeded,
    /// Pattern evaluation examined more terms than its admitted range budget.
    #[error("lexical term examination budget exceeded")]
    TermExaminationBudgetExceeded,
    /// The query's posting lists exceeded the admitted execution work.
    #[error("lexical posting budget exceeded")]
    PostingBudgetExceeded,
    /// Materialized result metadata exceeded its admitted text budget.
    #[error("lexical returned text budget exceeded")]
    ReturnedTextBudgetExceeded,
    /// The caller asked for an invalid result count.
    #[error("lexical result limit is invalid")]
    InvalidResultLimit,
    /// A caller-supplied query budget exceeded the implementation ceiling.
    #[error("lexical query budget is invalid: {resource}")]
    InvalidQueryBudget {
        /// Stable resource label.
        resource: &'static str,
    },
    /// The index belongs to a different immutable generation.
    #[error("lexical index generation does not match the requested generation")]
    GenerationMismatch {
        /// Requested generation.
        expected: GenerationId,
        /// Generation recorded by the index.
        actual: GenerationId,
    },
    /// On-disk schema, metadata, or document count is incompatible.
    #[error("lexical index format is incompatible")]
    IncompatibleIndex,
    /// The immutable artifact does not match its trusted manifest.
    #[error("lexical artifact integrity does not match its manifest")]
    ArtifactIntegrityMismatch,
    /// The artifact tree contains a link, special file, or non-portable name.
    #[error("lexical artifact tree is insecure")]
    InsecureArtifact,
    /// The staging directory was not empty and private to this build.
    #[error("lexical staging directory is not empty")]
    NonEmptyStaging,
    /// The private-file boundary is disabled.
    #[error("lexical private-file boundary is unsupported")]
    UnsupportedPrivateFileBoundary,
    /// A redacted storage or indexing operation failed.
    #[error("lexical index operation failed: {operation}")]
    IndexOperation {
        /// Stable operation label without paths or source payloads.
        operation: &'static str,
    },
}

impl From<rootlight_cancel::Cancelled> for SearchError {
    fn from(cancelled: rootlight_cancel::Cancelled) -> Self {
        Self::Cancelled(cancelled.reason())
    }
}
