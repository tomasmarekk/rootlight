//! Typed literal matching over one immutable source without returning its body.
//! Admission checks physical coordinates and continuation consistency; the daemon
//! verifies actual bytes, while higher-level callers authenticate cursor identity.

use super::*;
use rootlight_protocol::{
    DEFAULT_SOURCE_FIND_MATCHES, MAX_SOURCE_FIND_LITERAL_BYTES, MAX_SOURCE_FIND_MATCHES,
    MAX_SOURCE_FIND_SCAN_BYTES, SOURCE_FIND_PROTOCOL_MINOR,
};

/// Low-level continuation parts revalidated against source bytes by the daemon.
///
/// Callers must bind these parts to the prior repository, exact generation,
/// file, content hash, literal and controls. This type is not an authenticated
/// opaque cursor, and an active-generation selector cannot resume it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct SourceFindCursor {
    /// Next original byte at a Unicode scalar boundary.
    pub next_byte: u64,
    /// Literal scalars matched immediately before that byte.
    pub matched_prefix: u64,
}

/// Explicit forward-work and result bounds, independent of source-read bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceFindOptions {
    /// Prior cursor for the same immutable request, or the beginning of source.
    pub cursor: Option<SourceFindCursor>,
    /// Maximum original source bytes examined forward on this page.
    pub maximum_scan_bytes: u64,
    /// Maximum returned match references, including overlapping matches.
    pub maximum_matches: u32,
}

impl SourceFindOptions {
    /// Creates a first page with the protocol's scan and match defaults.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            cursor: None,
            maximum_scan_bytes: MAX_SOURCE_FIND_SCAN_BYTES,
            maximum_matches: DEFAULT_SOURCE_FIND_MATCHES,
        }
    }
}

impl Default for SourceFindOptions {
    fn default() -> Self {
        Self::new()
    }
}

/// Why a source-find page ended; only EOF establishes complete forward coverage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceFindStop {
    /// The selected immutable source was examined through its end.
    EndOfSource,
    /// The next complete scalar could not fit the forward scan allowance.
    ScanLimit,
    /// The requested number of match references was reached.
    MatchLimit,
    /// The independent matcher-memory allowance filled.
    MatcherMemoryLimit,
    /// The response-memory allowance filled.
    ResponseMemoryLimit,
    /// A lower explicit transport result allowance was reached.
    ResultLimit,
}

/// Shared generation-owned metadata for exact match references.
///
/// Path and language remain untrusted repository data. Mutable DTO fields are
/// checked on receipt and do not remain validation proofs after caller mutation.
#[derive(Clone, PartialEq, Eq, serde::Serialize)]
pub struct SourceFindFile {
    /// Selected immutable file identity.
    pub file: FileId,
    /// Repository-relative display path.
    pub path: String,
    /// Normalized indexed language identity.
    pub language: String,
    /// Hash of the entire physical file, not of any returned match.
    pub content_hash: ContentHash,
    /// Entire physical byte length, including an encoding BOM.
    pub byte_length: u64,
    /// Encoding used for every original-byte coordinate.
    pub encoding: SourceTextEncoding,
    /// Whether the generation marks this source as generated.
    pub generated: bool,
    /// Whole immediate container dependency for an extracted source.
    pub extracted_from: Option<SourceReference>,
}

impl std::fmt::Debug for SourceFindFile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SourceFindFile")
            .field("file", &self.file)
            .field("content_hash", &self.content_hash)
            .field("byte_length", &self.byte_length)
            .field("encoding", &self.encoding)
            .field("generated", &self.generated)
            .finish_non_exhaustive()
    }
}

/// Checked source-free match evidence and explicit physical scan coverage.
///
/// No unexamined total or structural/semantic completeness is inferred. The
/// source service authenticates physical bytes; this client checks correlation
/// and observable bounds without fetching those bytes again.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SourceFindResult {
    /// Selected repository identity.
    pub repository: RepositoryId,
    /// Exact immutable generation owning every returned reference.
    pub generation: GenerationId,
    /// Shared verified file metadata.
    pub file: SourceFindFile,
    /// Ordered exact physical matches; overlaps are retained.
    pub matches: Vec<SourceReference>,
    /// Forward byte coverage, excluding continuation restoration.
    pub scanned: std::ops::Range<u64>,
    /// Preceding physical bytes examined to restore the incoming cursor.
    pub restoration_bytes: u64,
    /// Authoritative end-of-source or resource boundary.
    pub stop: SourceFindStop,
    /// Positive-progress continuation when source remains unexamined.
    pub continuation: Option<SourceFindCursor>,
    /// Exact shared path and language byte lengths.
    pub total_metadata_bytes: u64,
    /// Producer-accounted owned response capacity, including match references.
    pub total_response_memory_bytes: u64,
}

impl Client {
    /// Finds case-sensitive literal matches in one immutable generation-owned file.
    ///
    /// # Errors
    /// Returns [`ClientError`] for invalid controls, unavailable protocol 1.27 or
    /// capability, transport failure, or inconsistent response evidence.
    pub fn source_find(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        file: FileId,
        literal: &str,
    ) -> Result<SourceFindResult, ClientError> {
        self.source_find_with_options(
            repository,
            generation,
            file,
            literal,
            SourceFindOptions::new(),
            RequestOptions::new(),
        )
    }

    /// Finds exact literals with independent scan, match and transport controls.
    ///
    /// # Errors
    /// Returns [`ClientError`] for invalid controls or unpinned continuation,
    /// unsupported protocol/capability, transport failure, or malformed evidence.
    pub fn source_find_with_options(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        file: FileId,
        literal: &str,
        search: SourceFindOptions,
        options: RequestOptions,
    ) -> Result<SourceFindResult, ClientError> {
        let (response, minor) = self.request_with_protocol_and_options(
            build_request(repository, generation, file, literal, search)?,
            options,
        )?;
        match response {
            daemon::response_envelope::Response::SourceFind(response) => parse_response(
                response, repository, generation, file, literal, search, options, minor,
            ),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Asynchronously finds literals with explicit scan and transport controls.
    ///
    /// Dropping the future closes its request stream and permits cancellation.
    ///
    /// # Panics
    /// Panics when polled without Tokio's time or I/O drivers enabled.
    ///
    /// # Errors
    /// Returns [`ClientError`] for invalid controls, unavailable protocol support,
    /// transport failure, timeout, or inconsistent response evidence.
    pub async fn source_find_async_with_options(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        file: FileId,
        literal: &str,
        search: SourceFindOptions,
        options: RequestOptions,
    ) -> Result<SourceFindResult, ClientError> {
        let (response, minor) = self
            .request_async_with_options_and_protocol(
                build_request(repository, generation, file, literal, search)?,
                options,
            )
            .await?;
        match response {
            daemon::response_envelope::Response::SourceFind(response) => parse_response(
                response, repository, generation, file, literal, search, options, minor,
            ),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }
}

fn validate_request(
    generation: GenerationSelector,
    literal: &str,
    search: SourceFindOptions,
) -> Result<(), ClientError> {
    if literal.is_empty()
        || literal.len() > MAX_SOURCE_FIND_LITERAL_BYTES
        || !(1..=MAX_SOURCE_FIND_SCAN_BYTES).contains(&search.maximum_scan_bytes)
        || !(1..=MAX_SOURCE_FIND_MATCHES).contains(&search.maximum_matches)
        || search.cursor.is_some_and(|cursor| {
            generation == GenerationSelector::Active
                || cursor.matched_prefix >= literal.chars().count() as u64
                || cursor.matched_prefix > cursor.next_byte
        })
    {
        return Err(ClientError::InvalidFirstSliceRequest);
    }
    Ok(())
}

fn build_request(
    repository: RepositoryId,
    generation: GenerationSelector,
    file: FileId,
    literal: &str,
    search: SourceFindOptions,
) -> Result<daemon::request_envelope::Request, ClientError> {
    validate_request(generation, literal, search)?;
    let mut query = String::new();
    query
        .try_reserve_exact(literal.len())
        .map_err(|_| ClientError::ResponseAllocationFailed)?;
    query.push_str(literal);
    Ok(daemon::request_envelope::Request::SourceFind(
        daemon::SourceFindRequest {
            schema_version: Some(first_slice_schema()),
            repository: Some(repository_to_wire(repository)),
            generation: Some(generation_selector_to_wire(generation)),
            file: Some(file_to_wire(file)),
            literal: query,
            cursor: search.cursor.map(|cursor| daemon::SourceFindCursor {
                next_byte: cursor.next_byte,
                matched_prefix: cursor.matched_prefix,
            }),
            maximum_scan_bytes: search.maximum_scan_bytes,
            maximum_matches: search.maximum_matches,
        },
    ))
}

fn parse_reference(
    reference: daemon::FirstSliceSourceRef,
    repository: RepositoryId,
    generation: GenerationId,
) -> Result<SourceReference, ClientError> {
    if parse_repository(
        reference
            .repository
            .ok_or(ClientError::InvalidResponseCorrelation)?,
    )? != repository
        || parse_generation(
            reference
                .generation
                .ok_or(ClientError::InvalidResponseCorrelation)?,
        )? != generation
    {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    SourceReference::new(
        repository,
        generation,
        parse_file(reference.file)?,
        reference.start_byte..reference.end_byte,
        parse_content_hash(reference.content_hash)?,
        match (reference.start_line, reference.end_line) {
            (None, None) => None,
            (Some(start), Some(end)) => Some(start..=end),
            _ => return Err(ClientError::InvalidResponseCorrelation),
        },
    )
    .map_err(|_| ClientError::InvalidResponseCorrelation)
}

fn encoded_width(literal: &str, encoding: SourceTextEncoding) -> u64 {
    match encoding {
        SourceTextEncoding::Utf8 => literal.len() as u64,
        SourceTextEncoding::Utf16Le | SourceTextEncoding::Utf16Be => {
            literal.encode_utf16().count() as u64 * 2
        }
    }
}

fn prefix_width(literal: &str, encoding: SourceTextEncoding, scalars: u64) -> u64 {
    literal
        .chars()
        .take(usize::try_from(scalars).unwrap_or(usize::MAX))
        .map(|character| {
            if encoding == SourceTextEncoding::Utf8 {
                character.len_utf8() as u64
            } else {
                character.len_utf16() as u64 * 2
            }
        })
        .sum()
}

#[expect(
    clippy::too_many_arguments,
    reason = "each argument binds a distinct request or response allowance"
)]
fn parse_response(
    response: daemon::SourceFindResponse,
    repository: RepositoryId,
    selector: GenerationSelector,
    requested_file: FileId,
    literal: &str,
    search: SourceFindOptions,
    options: RequestOptions,
    minor: u32,
) -> Result<SourceFindResult, ClientError> {
    validate_request(selector, literal, search)?;
    if minor < SOURCE_FIND_PROTOCOL_MINOR {
        return Err(ClientError::ProtocolFeatureUnavailable);
    }
    require_first_slice_response_schema(response.schema_version)?;
    let actual_repository = parse_repository(
        response
            .repository
            .ok_or(ClientError::InvalidResponseCorrelation)?,
    )?;
    let generation = parse_generation(
        response
            .generation
            .ok_or(ClientError::InvalidResponseCorrelation)?,
    )?;
    if actual_repository != repository
        || matches!(selector, GenerationSelector::Generation(expected) if expected != generation)
    {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    let file = response
        .file
        .ok_or(ClientError::InvalidResponseCorrelation)?;
    let file_id = parse_file(file.file)?;
    let hash = parse_content_hash(file.content_hash)?;
    let encoding = match daemon::SourcePhysicalEncoding::try_from(file.encoding) {
        Ok(daemon::SourcePhysicalEncoding::Utf8) => SourceTextEncoding::Utf8,
        Ok(daemon::SourcePhysicalEncoding::Utf16Le) => SourceTextEncoding::Utf16Le,
        Ok(daemon::SourcePhysicalEncoding::Utf16Be) => SourceTextEncoding::Utf16Be,
        Err(_) => return Err(ClientError::InvalidResponseCorrelation),
    };
    let content_start = if encoding == SourceTextEncoding::Utf8 {
        0
    } else {
        2
    };
    let width = encoded_width(literal, encoding);
    let scalars = literal.chars().count() as u64;
    let start = search
        .cursor
        .map_or(content_start, |cursor| cursor.next_byte);
    let prefix_bytes = search.cursor.map_or(0, |cursor| {
        prefix_width(literal, encoding, cursor.matched_prefix)
    });
    let end = response.scanned_end_byte;
    let metadata_bytes = file.path.len() as u64 + file.language.len() as u64;
    if file_id != requested_file
        || file.path.is_empty()
        || file.path.len() > rootlight_vfs::MAX_PATH_BYTES * 2
        || file.path.bytes().any(|byte| matches!(byte, b'\0' | b'\\'))
        || file
            .path
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
        || file.path.as_bytes().get(1) == Some(&b':')
        || file.language.is_empty()
        || file.language.len() > 256
        || !file.language.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'_' | b'-' | b'.' | b'+' | b'#')
        })
        || file.byte_length < content_start
        || start < content_start
        || start != response.scanned_start_byte
        || end < start
        || end > file.byte_length
        || end - start > search.maximum_scan_bytes
        || encoding != SourceTextEncoding::Utf8
            && [start, end, file.byte_length, response.restoration_bytes]
                .into_iter()
                .any(|byte| byte % 2 != 0)
        || response.matches.len() as u64 > u64::from(search.maximum_matches)
        || response.restoration_bytes > width.min(start - content_start)
        || response.restoration_bytes < prefix_bytes
        || search.cursor.is_none() && response.restoration_bytes != 0
        || response.total_metadata_bytes != metadata_bytes
        || response.total_response_memory_bytes < metadata_bytes
        || response.total_response_memory_bytes
            > options
                .effective_budget()
                .map_or(MAX_FIRST_SLICE_BUDGET_MEMORY_BYTES, |budget| {
                    budget.limits().memory_bytes
                })
    {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    let stop = match daemon::SourceFindStop::try_from(response.stop) {
        Ok(daemon::SourceFindStop::EndOfSource) => SourceFindStop::EndOfSource,
        Ok(daemon::SourceFindStop::ScanLimit) => SourceFindStop::ScanLimit,
        Ok(daemon::SourceFindStop::MatchLimit) => SourceFindStop::MatchLimit,
        Ok(daemon::SourceFindStop::MatcherMemoryLimit) => SourceFindStop::MatcherMemoryLimit,
        Ok(daemon::SourceFindStop::ResponseMemoryLimit) => SourceFindStop::ResponseMemoryLimit,
        Ok(daemon::SourceFindStop::ResultLimit) => SourceFindStop::ResultLimit,
        Err(_) => return Err(ClientError::InvalidResponseCorrelation),
    };
    let continuation = response.continuation.map(|cursor| SourceFindCursor {
        next_byte: cursor.next_byte,
        matched_prefix: cursor.matched_prefix,
    });
    if stop == SourceFindStop::EndOfSource {
        if end != file.byte_length || continuation.is_some() {
            return Err(ClientError::InvalidResponseCorrelation);
        }
    } else if end <= start
        || end >= file.byte_length
        || !continuation.is_some_and(|cursor| {
            cursor.next_byte == end
                && cursor.matched_prefix < scalars
                && prefix_width(literal, encoding, cursor.matched_prefix) <= end - content_start
        })
    {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    let effective_matches =
        options
            .effective_budget()
            .map_or(u64::from(search.maximum_matches), |budget| {
                budget
                    .limits()
                    .results
                    .min(budget.limits().rows.saturating_sub(1))
                    .min(u64::from(search.maximum_matches))
            });
    if effective_matches == 0
        || response.matches.len() as u64 > effective_matches
        || stop == SourceFindStop::MatchLimit
            && (effective_matches != u64::from(search.maximum_matches)
                || response.matches.len() as u64 != effective_matches)
        || stop == SourceFindStop::ResultLimit
            && (effective_matches >= u64::from(search.maximum_matches)
                || response.matches.len() as u64 != effective_matches)
        || matches!(
            stop,
            SourceFindStop::MatcherMemoryLimit | SourceFindStop::ResponseMemoryLimit
        ) && (response.matches.is_empty() || response.matches.len() as u64 >= effective_matches)
        || stop == SourceFindStop::ScanLimit
            && (search.maximum_scan_bytes - (end - start) >= 4
                || response.matches.len() as u64 >= effective_matches)
    {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    let extracted_from = file
        .extracted_from
        .map(|origin| {
            let origin = parse_reference(origin, repository, generation)?;
            if origin.file == file_id || origin.start_byte != 0 || origin.end_byte == 0 {
                return Err(ClientError::InvalidResponseCorrelation);
            }
            Ok(origin)
        })
        .transpose()?;
    let mut matches = Vec::new();
    matches
        .try_reserve_exact(response.matches.len())
        .map_err(|_| ClientError::ResponseAllocationFailed)?;
    let mut previous_end = start;
    for reference in response.matches {
        let reference = parse_reference(reference, repository, generation)?;
        if reference.file != file_id
            || reference.content_hash != hash
            || reference.end_byte - reference.start_byte != width
            || reference.start_byte < content_start.max(start.saturating_sub(prefix_bytes))
            || reference.end_byte <= previous_end
            || reference.end_byte > end
            || encoding != SourceTextEncoding::Utf8 && reference.start_byte % 2 != 0
        {
            return Err(ClientError::InvalidResponseCorrelation);
        }
        previous_end = reference.end_byte;
        matches.push(reference);
    }
    if matches!(
        stop,
        SourceFindStop::MatchLimit
            | SourceFindStop::MatcherMemoryLimit
            | SourceFindStop::ResponseMemoryLimit
            | SourceFindStop::ResultLimit
    ) && previous_end != end
    {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    Ok(SourceFindResult {
        repository,
        generation,
        file: SourceFindFile {
            file: file_id,
            path: file.path,
            language: file.language,
            content_hash: hash,
            byte_length: file.byte_length,
            encoding,
            generated: file.generated,
            extracted_from,
        },
        matches,
        scanned: start..end,
        restoration_bytes: response.restoration_bytes,
        stop,
        continuation,
        total_metadata_bytes: metadata_bytes,
        total_response_memory_bytes: response.total_response_memory_bytes,
    })
}

#[cfg(test)]
mod tests;
