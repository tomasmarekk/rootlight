//! Exact literal matching within one immutable, known source file.
//!
//! Scan completion describes physical source work only. It makes no claim
//! about language analysis, token matching, or repository-wide discovery.

use std::fmt;

use rootlight_ids::{ContentHash, FileId, GenerationId, RepositoryId};
use rootlight_ir::SourceRef;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::pagination::{AuthenticatedCursor, CursorContext, CursorError};
use crate::source_entity::EntityToolResponse;
use crate::source_text::SourceTextEncoding;
use crate::vertical::{
    ContinuationCursor, GenerationSelector, RepositorySelector, RequiredNullable,
    ResolvedRepository, ResponseBudget, ResponseProfile, UsageSummary,
};
use crate::{ExposureProfile, McpTool, TrustClassification};

/// Exact-literal contract with authenticated verification-duration provenance.
pub const SOURCE_FIND_VERSION: &str = "1.1";

/// Exact source-find response revision with verification-duration provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum SourceFindSchemaVersion {
    /// Source find contract 1.1.
    #[serde(rename = "1.1")]
    V1_1,
}

/// Maximum UTF-8 query bytes, without normalization or case folding.
pub const MAX_SOURCE_FIND_QUERY_BYTES: usize = 65_536;
/// Default and maximum forward physical bytes examined per page.
pub const MAX_SOURCE_FIND_SCAN_BYTES: u64 = 67_108_864;
/// Default number of matches returned per page.
pub const DEFAULT_SOURCE_FIND_MATCHES: u16 = 128;
/// Public maximum matches per page; response limits can reduce this further.
pub const MAX_SOURCE_FIND_MATCHES: u16 = 1_024;

/// Strict source.find input; the literal is never interpreted as a regex or token.
#[derive(Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceFindInput {
    /// Owning repository selected by its stable identity.
    pub repository: RepositorySelector,
    /// Active or omitted on the first page; exact generation on continuation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<GenerationSelector>,
    /// Exact indexed file identity; caller paths are not accepted.
    pub file_id: FileId,
    /// Nonempty exact scalar sequence, limited to 65536 UTF-8 bytes.
    #[schemars(length(min = 1, max = 65_536))]
    pub literal: String,
    /// Forward physical bytes per page; omitted means 64 MiB.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 67_108_864))]
    pub maximum_scan_bytes: Option<u64>,
    /// Matches per page; omitted means 128. Overlapping matches are included.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 1024))]
    pub maximum_matches: Option<u16>,
    /// Authenticated continuation preserving an unfinished literal prefix.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<ContinuationCursor>,
    /// Lower result, token, or timeout ceilings; other controls are unsupported.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<SourceFindResponseBudget>")]
    pub budget: Option<ResponseBudget>,
    /// Optional compact representation; source identity is always retained.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<SourceFindResponseProfile>")]
    pub response_profile: Option<ResponseProfile>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SourceFindResponseProfile {
    Compact,
}

impl JsonSchema for SourceFindResponseProfile {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "SourceFindResponseProfile".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        // The public schema validator admits const rather than enum constraints.
        schemars::json_schema!({ "type": "string", "const": "compact" })
    }
}

// Preserve the common Rust budget type while advertising only controls whose
// semantics apply to a source-only scan. Invariants reject the other fields.
#[derive(Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SourceFindResponseBudget {
    #[schemars(range(min = 1, max = 1000))]
    max_results: Option<u16>,
    #[schemars(range(min = 100, max = 16_000))]
    max_tokens: Option<u16>,
    #[schemars(range(min = 10, max = 30_000))]
    timeout_ms: Option<u32>,
}

impl fmt::Debug for SourceFindInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceFindInput")
            .field("file_id", &self.file_id)
            .field("literal_bytes", &self.literal.len())
            .field("has_cursor", &self.cursor.is_some())
            .finish_non_exhaustive()
    }
}

impl SourceFindInput {
    /// Returns the defaulted forward physical-byte ceiling.
    #[must_use]
    pub fn effective_maximum_scan_bytes(&self) -> u64 {
        self.maximum_scan_bytes
            .unwrap_or(MAX_SOURCE_FIND_SCAN_BYTES)
    }

    /// Returns the defaulted caller match ceiling, before transport limits.
    #[must_use]
    pub fn effective_maximum_matches(&self) -> u16 {
        self.maximum_matches.unwrap_or(DEFAULT_SOURCE_FIND_MATCHES)
    }

    /// Validates byte limits and continuation identity before source access.
    ///
    /// # Errors
    /// Returns a source-free error for invalid limits or a non-exact continuation.
    pub fn validate(&self) -> Result<(), SourceFindContractError> {
        validate_literal(&self.literal)?;
        if !matches!(self.repository, RepositorySelector::ById(_))
            || !(1..=MAX_SOURCE_FIND_SCAN_BYTES).contains(&self.effective_maximum_scan_bytes())
            || !(1..=MAX_SOURCE_FIND_MATCHES).contains(&self.effective_maximum_matches())
            || self
                .response_profile
                .is_some_and(|profile| profile != ResponseProfile::Compact)
            || (self.cursor.is_some()
                && !matches!(self.generation, Some(GenerationSelector::Explicit(_))))
        {
            return Err(SourceFindContractError::InvalidInput);
        }
        if let Some(budget) = &self.budget
            && (budget
                .max_results
                .is_some_and(|value| !(1..=1000).contains(&value))
                || budget
                    .max_tokens
                    .is_some_and(|value| !(100..=16_000).contains(&value))
                || budget
                    .timeout_ms
                    .is_some_and(|value| !(10..=30_000).contains(&value))
                || budget.max_source_bytes.is_some()
                || budget.max_traversal_facts.is_some()
                || budget.max_depth.is_some()
                || budget.max_paths.is_some()
                || budget.evidence_level.is_some())
        {
            return Err(SourceFindContractError::InvalidInput);
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for SourceFindInput {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            repository: RepositorySelector,
            generation: Option<GenerationSelector>,
            file_id: FileId,
            literal: String,
            maximum_scan_bytes: Option<u64>,
            maximum_matches: Option<u16>,
            cursor: Option<ContinuationCursor>,
            budget: Option<ResponseBudget>,
            response_profile: Option<SourceFindResponseProfile>,
        }
        let wire = Wire::deserialize(deserializer)?;
        let value = Self {
            repository: wire.repository,
            generation: wire.generation,
            file_id: wire.file_id,
            literal: wire.literal,
            maximum_scan_bytes: wire.maximum_scan_bytes,
            maximum_matches: wire.maximum_matches,
            cursor: wire.cursor,
            budget: wire.budget,
            response_profile: wire
                .response_profile
                .map(|SourceFindResponseProfile::Compact| ResponseProfile::Compact),
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

/// Single shared file identity for all original-byte match references.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceFindFile {
    /// Indexed file identity.
    pub id: FileId,
    /// Repository-relative display path, never a host selector.
    #[schemars(length(min = 1, max = 65_536))]
    pub path: String,
    /// Indexed language identity without a support-level claim.
    #[schemars(length(min = 1, max = 256))]
    pub language: String,
    /// Hash of the complete original physical source.
    pub content_hash: ContentHash,
    /// Complete physical source length including any BOM.
    pub byte_length: u64,
    /// Original physical encoding.
    pub encoding: SourceTextEncoding,
    /// Indexed generated-file marker.
    pub generated: bool,
    /// Mandatory classification of repository-controlled metadata.
    pub trust: TrustClassification,
    /// Whole immediate physical container of an extracted source, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extracted_from: Option<SourceRef>,
}

impl fmt::Debug for SourceFindFile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceFindFile")
            .field("id", &self.id)
            .field("content_hash", &self.content_hash)
            .field("byte_length", &self.byte_length)
            .field("encoding", &self.encoding)
            .finish_non_exhaustive()
    }
}

impl SourceFindFile {
    /// Computes the source identity bound into a continuation's authenticated plan.
    ///
    /// # Errors
    /// Returns a source-free error for inconsistent source metadata or provenance.
    pub fn fingerprint(
        &self,
        repository: RepositoryId,
        generation: GenerationId,
    ) -> Result<[u8; 32], SourceFindContractError> {
        if !self.validate(repository, generation) {
            return Err(SourceFindContractError::InvalidResponse);
        }
        let mut hash = blake3::Hasher::new();
        hash.update(b"rootlight.source.find.file.1.0\0");
        let file =
            serde_json::to_vec(self).map_err(|_| SourceFindContractError::InvalidResponse)?;
        hash.update(&file);
        Ok(*hash.finalize().as_bytes())
    }

    fn validate(&self, repository: RepositoryId, generation: GenerationId) -> bool {
        !self.path.is_empty()
            && self.path.len() <= 65_536
            && !self.path.bytes().any(|byte| matches!(byte, b'\0' | b'\\'))
            && !self
                .path
                .split('/')
                .any(|part| part.is_empty() || matches!(part, "." | ".."))
            && self.path.as_bytes().get(1) != Some(&b':')
            && !self.language.is_empty()
            && self.language.len() <= 256
            && self.language.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'_' | b'-' | b'.' | b'+' | b'#')
            })
            && (self.encoding == SourceTextEncoding::Utf8
                || (self.byte_length >= 2 && self.byte_length.is_multiple_of(2)))
            && self.extracted_from.as_ref().is_none_or(|origin| {
                origin.repository() == repository
                    && origin.generation() == generation
                    && origin.span().file() != self.id
                    && origin.span().start_byte() == 0
                    && origin.span().end_byte() > 0
            })
    }

    fn valid_byte_boundary(&self, offset: u64) -> bool {
        offset <= self.byte_length
            && (self.encoding == SourceTextEncoding::Utf8
                || (offset >= 2 && offset.is_multiple_of(2)))
    }
}

/// Actual reason a page stopped; only EndOfSource completes the scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceFindStop {
    /// The forward scan reached the complete physical source end.
    EndOfSource,
    /// The requested forward physical-byte ceiling was reached.
    ScanLimit,
    /// The caller's maximum_matches ceiling was reached.
    MatchLimit,
    /// Internal matcher memory limited the returned page.
    MatcherMemoryLimit,
    /// Owned source-response memory limited the returned page.
    ResponseMemoryLimit,
    /// A transport result ceiling below maximum_matches stopped the page.
    ResultLimit,
}

/// One bounded page; match bodies and unknown total counts are never copied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceFindData {
    /// Shared physical source identity and repository metadata.
    pub file: SourceFindFile,
    /// Exact matches in strictly increasing start order, including overlaps.
    #[schemars(length(max = 1024))]
    pub matches: Vec<SourceRef>,
    /// Inclusive original-byte forward scan start; UTF-16 begins after its BOM.
    pub scanned_start_byte: u64,
    /// Exclusive original-byte forward scan end.
    pub scanned_end_byte: u64,
    /// Independently charged lookbehind used to validate a restored prefix.
    pub restoration_bytes: u64,
    /// Actual stop boundary, distinct from whole-language completeness.
    pub stop: SourceFindStop,
    /// Exact UTF-8 byte lengths of the shared path and language metadata.
    pub total_metadata_bytes: u64,
    /// Owned source-response allocation, excluding matched bodies (never copied).
    pub total_response_memory_bytes: u64,
}

/// Generation-pinned source-only response, without structural coverage claims.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceFindEnvelope {
    /// Exact tool contract revision.
    pub schema_version: SourceFindSchemaVersion,
    /// Resolved repository identity.
    pub repository: ResolvedRepository,
    /// Immutable generation used for every page and source reference.
    pub generation: GenerationId,
    /// Bounded physical source work and exact matches.
    pub data: SourceFindData,
    /// True exactly when the page stopped before end of source.
    pub truncated: bool,
    /// Authenticated continuation, absent exactly at end of source.
    pub next_cursor: RequiredNullable<ContinuationCursor>,
    /// Runtime response and execution accounting; source_bytes is zero.
    pub usage: UsageSummary,
    /// Classification for all repository-derived metadata and references.
    pub trust: TrustClassification,
}

impl SourceFindEnvelope {
    /// Rejects contradictory completion, foreign references, or reordered hits.
    ///
    /// # Errors
    /// Returns a source-free error when the response misstates its source work.
    pub fn validate(&self) -> Result<(), SourceFindContractError> {
        let data = &self.data;
        let file = &data.file;
        let eof = data.stop == SourceFindStop::EndOfSource;
        let content_start = if file.encoding == SourceTextEncoding::Utf8 {
            0
        } else {
            2
        };
        if !file.validate(self.repository.repository_id, self.generation)
            || data.matches.len() > usize::from(MAX_SOURCE_FIND_MATCHES)
            || !file.valid_byte_boundary(data.scanned_start_byte)
            || !file.valid_byte_boundary(data.scanned_end_byte)
            || data.scanned_start_byte > data.scanned_end_byte
            || data.total_metadata_bytes > data.total_response_memory_bytes
            || file
                .path
                .len()
                .checked_add(file.language.len())
                .and_then(|bytes| u64::try_from(bytes).ok())
                != Some(data.total_metadata_bytes)
            || data.restoration_bytes > data.scanned_start_byte.saturating_sub(content_start)
            || (file.encoding != SourceTextEncoding::Utf8
                && !data.restoration_bytes.is_multiple_of(2))
            || self.usage.source_bytes != 0
            || self.truncated == eof
            || self.next_cursor.0.is_some() == eof
            || (eof && data.scanned_end_byte != file.byte_length)
            || (!eof
                && (data.scanned_start_byte == data.scanned_end_byte
                    || data.scanned_end_byte == file.byte_length))
            || (data.matches.is_empty()
                && !matches!(
                    data.stop,
                    SourceFindStop::EndOfSource | SourceFindStop::ScanLimit
                ))
        {
            return Err(SourceFindContractError::InvalidResponse);
        }
        let mut previous_start = None;
        let mut previous_end = None;
        for source in &data.matches {
            let span = source.span();
            if source.repository() != self.repository.repository_id
                || source.generation() != self.generation
                || span.file() != file.id
                || source.content_hash() != file.content_hash
                || source.line_hint().is_some()
                || !file.valid_byte_boundary(span.start_byte())
                || !file.valid_byte_boundary(span.end_byte())
                || span.start_byte() >= span.end_byte()
                || span.end_byte() <= data.scanned_start_byte
                || span.end_byte() > data.scanned_end_byte
                || previous_start.is_some_and(|previous| previous >= span.start_byte())
                || previous_end.is_some_and(|previous| previous >= span.end_byte())
            {
                return Err(SourceFindContractError::InvalidResponse);
            }
            previous_start = Some(span.start_byte());
            previous_end = Some(span.end_byte());
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for SourceFindEnvelope {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            schema_version: SourceFindSchemaVersion,
            repository: ResolvedRepository,
            generation: GenerationId,
            data: SourceFindData,
            truncated: bool,
            next_cursor: RequiredNullable<ContinuationCursor>,
            usage: UsageSummary,
            trust: TrustClassification,
        }
        let wire = Wire::deserialize(deserializer)?;
        let value = Self {
            schema_version: wire.schema_version,
            repository: wire.repository,
            generation: wire.generation,
            data: wire.data,
            truncated: wire.truncated,
            next_cursor: wire.next_cursor,
            usage: wire.usage,
            trust: wire.trust,
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

/// Exact source-only success or the common versioned public error.
pub type SourceFindOutput = EntityToolResponse<SourceFindEnvelope, SourceFindSchemaVersion>;

/// Effective independent limits authenticated into every continuation.
///
/// Producers supply admitted values. Duration provenance is retained separately:
/// omission permits streamed verification without a synthetic default deadline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceFindCursorBudgets {
    /// Forward physical source bytes per page.
    pub maximum_scan_bytes: u64,
    /// Caller match ceiling before transport admission.
    pub maximum_matches: u16,
    /// UTF-8 query bytes.
    pub maximum_query_bytes: u64,
    /// Resident immutable snapshot memory, independent of streamed verification.
    pub maximum_snapshot_bytes: u64,
    /// Complete original bytes permitted for source authentication.
    pub maximum_verified_bytes: u64,
    /// Internal matcher allocation.
    pub maximum_matcher_memory_bytes: u64,
    /// Independent restored-prefix lookbehind bytes.
    pub maximum_restore_bytes: u64,
    /// Owned source-response memory.
    pub maximum_response_memory_bytes: u64,
    /// Source metadata bytes.
    pub maximum_metadata_bytes: u64,
    /// Effective transport result ceiling.
    pub maximum_results: u16,
    /// Encoded transport response bytes.
    pub maximum_json_bytes: u64,
    /// Estimated output tokens.
    pub maximum_tokens: u64,
    /// Effective duration when `source_verification_duration_explicit` is true.
    pub timeout_ms: u64,
    /// Whether the client explicitly selected a source verification duration.
    pub source_verification_duration_explicit: bool,
}

/// Authenticated scan position; source validation still verifies the prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceFindCursorPosition {
    /// Next physical byte to examine.
    pub next_byte: u64,
    /// Number of literal scalars in the unfinished matched prefix.
    pub matched_prefix: u64,
}

/// Complete immutable source and policy binding for an authenticated cursor.
pub struct SourceFindCursorBinding<'a> {
    /// Owning repository identity.
    pub repository: RepositoryId,
    /// Exact selected generation, including resumed pages.
    pub generation: GenerationId,
    /// Verified source identity, encoding, and provenance.
    pub file: &'a SourceFindFile,
    /// Exact unnormalized literal; only its digest enters the cursor.
    pub literal: &'a str,
    /// Admitted independent resource ceilings.
    pub budgets: &'a SourceFindCursorBudgets,
    /// Active representation profile.
    pub response_profile: ResponseProfile,
    /// Server-approved discovery profile.
    pub exposure_profile: ExposureProfile,
    /// Logical immutable generation snapshot identity.
    pub snapshot_id: [u8; 32],
    /// Non-secret signing-key identity.
    pub key_id: u64,
}

impl SourceFindCursorBinding<'_> {
    /// Computes the exact source/query/limit context without embedding source text.
    ///
    /// # Errors
    /// Returns a source-free error for an invalid query, source, or serialization.
    pub fn context(&self) -> Result<CursorContext, SourceFindContractError> {
        self.request()
            .context(self.file.fingerprint(self.repository, self.generation)?)
    }

    /// Authenticates a progressing non-EOF position for the current source.
    ///
    /// # Errors
    /// Returns a source-free error for an invalid position or signing envelope.
    pub fn issue(
        &self,
        position: SourceFindCursorPosition,
        issued_at_ms: u64,
        server_key: &[u8; 32],
    ) -> Result<ContinuationCursor, SourceFindContractError> {
        self.validate_position(position)?;
        let payload = [
            position.next_byte.to_le_bytes(),
            position.matched_prefix.to_le_bytes(),
        ]
        .concat();
        let cursor =
            AuthenticatedCursor::create(self.context()?, payload, issued_at_ms, server_key)?;
        ContinuationCursor::parse(&cursor.to_wire())
            .map_err(|_| SourceFindContractError::Cursor(CursorError::Malformed))
    }

    /// Verifies authentication and every binding before returning a scan position.
    ///
    /// # Errors
    /// Returns a source-free error for forgery, changed scope/limits, or expiry.
    /// The source matcher must independently validate the restored scalar prefix.
    pub fn resume(
        &self,
        cursor: &ContinuationCursor,
        now_ms: u64,
        server_key: &[u8; 32],
    ) -> Result<SourceFindCursorPosition, SourceFindContractError> {
        let resumed = self.request().resume(cursor, now_ms, server_key)?;
        if resumed.file_fingerprint != self.file.fingerprint(self.repository, self.generation)? {
            return Err(SourceFindContractError::Cursor(CursorError::QueryMismatch));
        }
        self.validate_position(resumed.position)?;
        Ok(resumed.position)
    }

    fn request(&self) -> SourceFindRequestBinding<'_> {
        SourceFindRequestBinding {
            repository: self.repository,
            generation: self.generation,
            file_id: self.file.id,
            literal: self.literal,
            budgets: self.budgets,
            response_profile: self.response_profile,
            exposure_profile: self.exposure_profile,
            snapshot_id: self.snapshot_id,
            key_id: self.key_id,
        }
    }

    fn validate_position(
        &self,
        position: SourceFindCursorPosition,
    ) -> Result<(), SourceFindContractError> {
        validate_literal(self.literal)?;
        let content_start = if self.file.encoding == SourceTextEncoding::Utf8 {
            0
        } else {
            2
        };
        let literal_scalars = u64::try_from(self.literal.chars().count())
            .map_err(|_| SourceFindContractError::InvalidInput)?;
        if position.next_byte <= content_start
            || position.next_byte >= self.file.byte_length
            || !self.file.valid_byte_boundary(position.next_byte)
            || position.matched_prefix >= literal_scalars
            || position.matched_prefix > position.next_byte - content_start
        {
            return Err(SourceFindContractError::Cursor(CursorError::Malformed));
        }
        Ok(())
    }
}

impl fmt::Debug for SourceFindCursorBinding<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceFindCursorBinding")
            .field("file", self.file)
            .field("literal_bytes", &self.literal.len())
            .finish_non_exhaustive()
    }
}

/// Request-only binding that can authenticate continuation before loading source.
pub struct SourceFindRequestBinding<'a> {
    /// Owning repository identity.
    pub repository: RepositoryId,
    /// Exact selected generation; active generation cannot resume a cursor.
    pub generation: GenerationId,
    /// Exact requested file identity.
    pub file_id: FileId,
    /// Unnormalized case-sensitive scalar literal.
    pub literal: &'a str,
    /// Admitted independent resource ceilings.
    pub budgets: &'a SourceFindCursorBudgets,
    /// Active representation profile.
    pub response_profile: ResponseProfile,
    /// Server-approved discovery profile.
    pub exposure_profile: ExposureProfile,
    /// Logical immutable generation snapshot identity.
    pub snapshot_id: [u8; 32],
    /// Non-secret signing-key identity.
    pub key_id: u64,
}

impl fmt::Debug for SourceFindRequestBinding<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceFindRequestBinding")
            .field("file_id", &self.file_id)
            .field("literal_bytes", &self.literal.len())
            .finish_non_exhaustive()
    }
}

/// Authenticated continuation awaiting comparison with the loaded source identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceFindResumedCursor {
    /// Prefix state still independently checked by the source matcher.
    pub position: SourceFindCursorPosition,
    /// Must equal the returned file's fingerprint before any result is emitted.
    pub file_fingerprint: [u8; 32],
}

impl SourceFindRequestBinding<'_> {
    /// Authenticates the entire request before any source access is needed.
    ///
    /// # Errors
    /// Returns a source-free error for forgery, changed request, expiry, or an
    /// invalid position. Callers must compare the returned file fingerprint
    /// with the actual source response before emitting matches or a new cursor.
    pub fn resume(
        &self,
        cursor: &ContinuationCursor,
        now_ms: u64,
        server_key: &[u8; 32],
    ) -> Result<SourceFindResumedCursor, SourceFindContractError> {
        let cursor = AuthenticatedCursor::from_wire(cursor.as_str())?;
        let fingerprint = cursor.plan_fingerprint();
        // This fingerprint is only an expected value until the MAC, request,
        // key, and lifetime checks authenticate the complete cursor envelope.
        cursor.validate(&self.context(fingerprint)?, now_ms, server_key)?;
        let payload: [u8; 16] = cursor
            .last_sort_key()
            .try_into()
            .map_err(|_| SourceFindContractError::Cursor(CursorError::Malformed))?;
        let (next, prefix) = payload.split_at(8);
        let position = SourceFindCursorPosition {
            next_byte: u64::from_le_bytes(
                next.try_into()
                    .map_err(|_| SourceFindContractError::Cursor(CursorError::Malformed))?,
            ),
            matched_prefix: u64::from_le_bytes(
                prefix
                    .try_into()
                    .map_err(|_| SourceFindContractError::Cursor(CursorError::Malformed))?,
            ),
        };
        let scalar_count = u64::try_from(self.literal.chars().count())
            .map_err(|_| SourceFindContractError::InvalidInput)?;
        if position.next_byte == 0
            || position.matched_prefix >= scalar_count
            || position.matched_prefix > position.next_byte
        {
            return Err(SourceFindContractError::Cursor(CursorError::Malformed));
        }
        Ok(SourceFindResumedCursor {
            position,
            file_fingerprint: fingerprint,
        })
    }

    fn context(
        &self,
        file_fingerprint: [u8; 32],
    ) -> Result<CursorContext, SourceFindContractError> {
        validate_literal(self.literal)?;
        if !(1..=MAX_SOURCE_FIND_SCAN_BYTES).contains(&self.budgets.maximum_scan_bytes)
            || !(1..=MAX_SOURCE_FIND_MATCHES).contains(&self.budgets.maximum_matches)
            || self.budgets.maximum_results == 0
        {
            return Err(SourceFindContractError::InvalidInput);
        }
        let mut query = blake3::Hasher::new();
        query.update(b"rootlight.source.find.scalar-kmp.query.1.1\0");
        query.update(blake3::hash(self.literal.as_bytes()).as_bytes());
        let fields = serde_json::to_vec(&(self.file_id, self.budgets))
            .map_err(|_| SourceFindContractError::InvalidInput)?;
        query.update(&fields);
        Ok(CursorContext {
            repository: self.repository,
            generation: self.generation,
            tool: McpTool::SourceFind,
            tool_major_version: 1,
            query_fingerprint: *query.finalize().as_bytes(),
            plan_fingerprint: file_fingerprint,
            response_profile: self.response_profile,
            exposure_profile: self.exposure_profile,
            snapshot_id: self.snapshot_id,
            page_size: self
                .budgets
                .maximum_matches
                .min(self.budgets.maximum_results),
            key_id: self.key_id,
        })
    }
}

/// Source-free validation failures for exact literal wire evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SourceFindContractError {
    /// The input has an invalid literal, limit, source identity, or resume selector.
    #[error("invalid source find input")]
    InvalidInput,
    /// The returned source identity, ordered matches, or completion is contradictory.
    #[error("invalid source find response")]
    InvalidResponse,
    /// The authenticated continuation could not be verified.
    #[error(transparent)]
    Cursor(#[from] CursorError),
}

fn validate_literal(literal: &str) -> Result<(), SourceFindContractError> {
    if literal.is_empty() || literal.len() > MAX_SOURCE_FIND_QUERY_BYTES {
        return Err(SourceFindContractError::InvalidInput);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
