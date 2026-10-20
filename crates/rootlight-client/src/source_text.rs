//! Lossless text transport preserving immutable physical source references.
//! Response admission checks scalar endpoints, encoding widths, and both byte totals.

use super::*;

#[cfg(test)]
#[path = "source_text_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "source_text_prefix_tests.rs"]
mod prefix_tests;

/// Physical encoding of the immutable source, not of the returned Rust string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceTextEncoding {
    /// Exact UTF-8 source bytes.
    Utf8,
    /// BOM-marked UTF-16 little-endian source bytes.
    Utf16Le,
    /// BOM-marked UTF-16 big-endian source bytes.
    Utf16Be,
}

/// Bounded lossless-text source-read presentation controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceTextReadOptions {
    /// Whole lines included before each selected range.
    pub context_lines_before: u8,
    /// Whole lines included after each selected range.
    pub context_lines_after: u8,
    /// Merge overlapping references to the same immutable file.
    pub merge_overlaps: bool,
    /// Include one-based line ranges in returned chunks.
    pub include_line_numbers: bool,
}

impl SourceTextReadOptions {
    /// Creates lossless reads with line numbers and no range expansion.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            context_lines_before: 0,
            context_lines_after: 0,
            merge_overlaps: false,
            include_line_numbers: true,
        }
    }
}

impl Default for SourceTextReadOptions {
    fn default() -> Self {
        Self::new()
    }
}

/// One lossless text presentation with original physical source coordinates.
///
/// The client checks returned records before exposing them. These public DTO
/// fields remain mutable; callers must not treat later mutations as verified.
#[derive(Clone, PartialEq, Eq, serde::Serialize)]
pub struct SourceTextChunk {
    /// Whole immediate container dependency; offsets address the derived source.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extracted_from: Option<SourceReference>,
    /// Exact immutable selection in original source bytes.
    pub source: SourceReference,
    /// Untrusted repository-relative path.
    pub path: String,
    /// Returned half-open physical byte range start.
    pub start_byte: u64,
    /// Returned half-open physical byte range end.
    pub end_byte: u64,
    /// Returned first one-based line.
    pub start_line: Option<u64>,
    /// Returned final one-based line.
    pub end_line: Option<u64>,
    /// Untrusted decoded text; UTF-8 string offsets are not source offsets.
    pub content: String,
    /// Original source encoding, including the selected BOM when present.
    pub physical_encoding: SourceTextEncoding,
    /// Correlated original-file digest, not a digest of decoded text.
    pub content_hash: ContentHash,
    /// Stable normalized language label.
    pub language: String,
    /// Authoritative analysis tier of the source file.
    pub tier: AnalysisTier,
    /// Whether generated-code policy applies.
    pub generated: bool,
}

impl std::fmt::Debug for SourceTextChunk {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SourceTextChunk")
            .field("source", &self.source)
            .field("start_byte", &self.start_byte)
            .field("end_byte", &self.end_byte)
            .field("start_line", &self.start_line)
            .field("end_line", &self.end_line)
            .field("text_bytes", &self.content.len())
            .field("physical_encoding", &self.physical_encoding)
            .field("content_hash", &self.content_hash)
            .field("language", &self.language)
            .field("tier", &self.tier)
            .field("generated", &self.generated)
            .finish_non_exhaustive()
    }
}

/// Bounded lossless-text response checked at the client boundary.
///
/// Public presentation fields may subsequently be mutated by callers.
#[derive(Clone, PartialEq, Eq, serde::Serialize)]
pub struct SourceTextReadResult {
    /// Checked query correlation.
    pub context: QueryContext,
    /// Checked text presentations in request order.
    pub chunks: Vec<SourceTextChunk>,
    /// Authenticated source-only recovery; absence makes no completeness claim.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_recovery: Option<SourceReadRecovery>,
    /// Total returned original source bytes.
    pub total_source_bytes: u64,
    /// Total returned UTF-8 presentation bytes.
    pub total_text_bytes: u64,
    /// Whether the response was cut by a bound.
    pub truncated: bool,
    /// Authoritative execution-completeness diagnostics.
    pub execution_completeness: ResultCompleteness,
    /// Ordered membership metadata for the actual expanded physical ranges.
    pub conditional_activity: Vec<ConditionalActivityAssessment>,
}

impl std::fmt::Debug for SourceTextReadResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SourceTextReadResult")
            .field("context", &self.context)
            .field("chunk_count", &self.chunks.len())
            .field("total_source_bytes", &self.total_source_bytes)
            .field("total_text_bytes", &self.total_text_bytes)
            .field("truncated", &self.truncated)
            .finish()
    }
}

/// Lossless prefixes retaining the complete approved source selections.
///
/// `prefix_truncated` describes intentional presentation clipping, independently
/// of execution truncation in `result`. Returned fields are mutable DTOs, not
/// persistent validation proofs; partial text does not authenticate the full hash.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SourceTextPrefixReadResult {
    /// Checked presentations and physical accounting for the returned prefixes.
    pub result: SourceTextReadResult,
    /// Whether any returned prefix omits part of its context-expanded range.
    pub prefix_truncated: bool,
}

impl Client {
    /// Reads immutable selections as lossless UTF-8 text without changing source offsets.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] for invalid references, unavailable protocol 1.24,
    /// transport failure, or malformed response metadata.
    pub fn source_read_lossless_text(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        references: &[SourceReference],
    ) -> Result<SourceTextReadResult, ClientError> {
        self.source_read_lossless_text_with_options(
            repository,
            generation,
            references,
            SourceTextReadOptions::new(),
            RequestOptions::new(),
        )
    }

    /// Reads lossless text with explicit presentation and transport controls.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] for invalid controls, unavailable protocol support,
    /// transport failure, or uncorrelated source/text metadata.
    pub fn source_read_lossless_text_with_options(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        references: &[SourceReference],
        projection: SourceTextReadOptions,
        options: RequestOptions,
    ) -> Result<SourceTextReadResult, ClientError> {
        let (response, minor) = self.request_with_protocol_and_options(
            build_source_text_read_request(repository, generation, references, projection)?,
            options,
        )?;
        match response {
            daemon::response_envelope::Response::SourceRead(response) => {
                parse_source_text_read_for_minor(
                    response, repository, generation, references, projection, minor,
                )
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Asynchronously reads lossless text with explicit presentation and transport controls.
    ///
    /// Dropping the future closes its one-request stream and permits daemon cancellation.
    ///
    /// # Panics
    ///
    /// Panics if polled without Tokio's time or I/O drivers enabled.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] for invalid controls, unavailable protocol support,
    /// transport failure, timeout, or malformed source/text metadata.
    pub async fn source_read_lossless_text_async_with_options(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        references: &[SourceReference],
        projection: SourceTextReadOptions,
        options: RequestOptions,
    ) -> Result<SourceTextReadResult, ClientError> {
        let (response, minor) = self
            .request_async_with_options_and_protocol(
                build_source_text_read_request(repository, generation, references, projection)?,
                options,
            )
            .await?;
        match response {
            daemon::response_envelope::Response::SourceRead(response) => {
                parse_source_text_read_for_minor(
                    response, repository, generation, references, projection, minor,
                )
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }
}

pub(super) fn build_source_text_read_request(
    repository: RepositoryId,
    generation: GenerationSelector,
    references: &[SourceReference],
    projection: SourceTextReadOptions,
) -> Result<daemon::request_envelope::Request, ClientError> {
    let request = build_source_read_request(
        repository,
        generation,
        references,
        SourceReadOptions {
            context_lines_before: projection.context_lines_before,
            context_lines_after: projection.context_lines_after,
            merge_overlaps: projection.merge_overlaps,
            include_line_numbers: projection.include_line_numbers,
            encoding: SourceEncoding::Utf8,
        },
    )?;
    match request {
        daemon::request_envelope::Request::SourceRead(mut request) => {
            request.encoding = daemon::SourceReadEncoding::LosslessText as i32;
            Ok(daemon::request_envelope::Request::SourceRead(request))
        }
        _ => Err(ClientError::InvalidFirstSliceRequest),
    }
}

impl Client {
    /// Reads scalar-bounded text prefixes without narrowing approved source references.
    ///
    /// Context lines are allowed; merging and line-number output must be disabled.
    /// The cap bounds UTF-8 presentation, not original physical source bytes.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] for invalid controls, unavailable protocol 1.25,
    /// transport failure, or inconsistent prefix metadata.
    pub fn source_read_lossless_text_prefix_with_options(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        references: &[SourceReference],
        projection: SourceTextReadOptions,
        maximum_text_bytes_per_chunk: u32,
        options: RequestOptions,
    ) -> Result<SourceTextPrefixReadResult, ClientError> {
        let (response, minor) = self.request_with_protocol_and_options(
            build_source_text_prefix_read_request(
                repository,
                generation,
                references,
                projection,
                maximum_text_bytes_per_chunk,
            )?,
            options,
        )?;
        match response {
            daemon::response_envelope::Response::SourceRead(response) => {
                parse_source_text_prefix_read_for_minor(
                    response,
                    repository,
                    generation,
                    references,
                    projection,
                    maximum_text_bytes_per_chunk,
                    minor,
                )
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Asynchronously reads bounded prefixes while preserving physical source identity.
    ///
    /// Dropping the future closes its one-request stream and permits daemon cancellation.
    /// Context lines are allowed; merging and line-number output must be disabled.
    ///
    /// # Panics
    ///
    /// Panics if polled without Tokio's time or I/O drivers enabled.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] for invalid controls, unavailable protocol 1.25,
    /// transport failure, timeout, or inconsistent prefix metadata.
    pub async fn source_read_lossless_text_prefix_async_with_options(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        references: &[SourceReference],
        projection: SourceTextReadOptions,
        maximum_text_bytes_per_chunk: u32,
        options: RequestOptions,
    ) -> Result<SourceTextPrefixReadResult, ClientError> {
        let (response, minor) = self
            .request_async_with_options_and_protocol(
                build_source_text_prefix_read_request(
                    repository,
                    generation,
                    references,
                    projection,
                    maximum_text_bytes_per_chunk,
                )?,
                options,
            )
            .await?;
        match response {
            daemon::response_envelope::Response::SourceRead(response) => {
                parse_source_text_prefix_read_for_minor(
                    response,
                    repository,
                    generation,
                    references,
                    projection,
                    maximum_text_bytes_per_chunk,
                    minor,
                )
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }
}

pub(super) fn build_source_text_prefix_read_request(
    repository: RepositoryId,
    generation: GenerationSelector,
    references: &[SourceReference],
    projection: SourceTextReadOptions,
    maximum_text_bytes_per_chunk: u32,
) -> Result<daemon::request_envelope::Request, ClientError> {
    if projection.merge_overlaps || projection.include_line_numbers {
        return Err(ClientError::InvalidFirstSliceRequest);
    }
    let request = build_source_text_read_request(repository, generation, references, projection)?;
    let daemon::request_envelope::Request::SourceRead(mut request) = request else {
        return Err(ClientError::InvalidFirstSliceRequest);
    };
    request.maximum_text_bytes_per_chunk = Some(maximum_text_bytes_per_chunk);
    validate_source_text_prefix_request(&request)?;
    Ok(daemon::request_envelope::Request::SourceRead(request))
}

pub(super) fn validate_source_text_prefix_request(
    request: &daemon::SourceReadRequest,
) -> Result<(), ClientError> {
    let cap = request
        .maximum_text_bytes_per_chunk
        .ok_or(ClientError::InvalidFirstSliceRequest)?;
    if cap == 0
        || u64::from(cap) > rootlight_protocol::MAX_FIRST_SLICE_BUDGET_SOURCE_BYTES
        || request.encoding != daemon::SourceReadEncoding::LosslessText as i32
        || request.merge_overlaps
        || request.include_line_numbers != Some(false)
    {
        return Err(ClientError::InvalidFirstSliceRequest);
    }
    Ok(())
}

pub(super) fn parse_source_text_read_for_minor(
    response: daemon::SourceReadResponse,
    repository: RepositoryId,
    selector: GenerationSelector,
    requested: &[SourceReference],
    projection: SourceTextReadOptions,
    selected_protocol_minor: u32,
) -> Result<SourceTextReadResult, ClientError> {
    if selected_protocol_minor < rootlight_protocol::LOSSLESS_SOURCE_TEXT_PROTOCOL_MINOR {
        return Err(ClientError::ProtocolFeatureUnavailable);
    }
    if response.prefix_truncated.is_some() {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    require_first_slice_response_schema(response.schema_version)?;
    let execution_completeness =
        parse_result_completeness(response.completeness, Some(response.truncated), false)?;
    let context = parse_query_context(response.context, repository, selector)?;
    let expected = if projection.merge_overlaps {
        merge_client_source_references(requested)?
    } else {
        requested.to_vec()
    };
    if context.generation
        != requested
            .first()
            .ok_or(ClientError::InvalidResponseCorrelation)?
            .generation
        || response.chunks.len() > expected.len()
        || (!response.truncated && response.chunks.len() != expected.len())
    {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    let mut chunks = Vec::new();
    chunks
        .try_reserve_exact(response.chunks.len())
        .map_err(|_| ClientError::ResponseAllocationFailed)?;
    let mut source_bytes = 0_u64;
    let mut text_bytes = 0_u64;
    for (chunk, expected) in response.chunks.into_iter().zip(&expected) {
        let start_line = chunk.included_start_line;
        let end_line = chunk.included_end_line;
        let source = parse_source_reference(
            chunk
                .source
                .ok_or(ClientError::InvalidResponseCorrelation)?,
            &context,
        )?;
        let content_hash = parse_content_hash(chunk.content_hash)?;
        let length = chunk
            .end_byte
            .checked_sub(chunk.start_byte)
            .ok_or(ClientError::InvalidResponseCorrelation)?;
        if &source != expected
            || chunk.start_byte > source.start_byte
            || chunk.end_byte < source.end_byte
            || projection.context_lines_before == 0 && chunk.start_byte != source.start_byte
            || projection.context_lines_after == 0 && chunk.end_byte != source.end_byte
            || projection.include_line_numbers
                && (start_line.is_none()
                    || end_line.is_none()
                    || start_line > end_line
                    || start_line == Some(0))
            || projection.include_line_numbers
                && source.line_range().is_some_and(|selected| {
                    start_line.is_some_and(|start| start > *selected.start())
                        || end_line.is_some_and(|end| end < *selected.end())
                })
            || !projection.include_line_numbers && (start_line.is_some() || end_line.is_some())
            || chunk.start_line != start_line.unwrap_or(0)
            || chunk.end_line != end_line.unwrap_or(0)
            || content_hash != source.content_hash
            || chunk.encoding != daemon::SourceReadEncoding::LosslessText as i32
            || !chunk.exact_content.is_empty()
        {
            return Err(ClientError::InvalidResponseCorrelation);
        }
        let physical_encoding = match chunk
            .physical_encoding
            .and_then(|encoding| daemon::SourcePhysicalEncoding::try_from(encoding).ok())
        {
            Some(daemon::SourcePhysicalEncoding::Utf8) => SourceTextEncoding::Utf8,
            Some(daemon::SourcePhysicalEncoding::Utf16Le) => SourceTextEncoding::Utf16Le,
            Some(daemon::SourcePhysicalEncoding::Utf16Be) => SourceTextEncoding::Utf16Be,
            None => return Err(ClientError::InvalidResponseCorrelation),
        };
        validate_text_coordinates(
            &chunk.content,
            physical_encoding,
            chunk.start_byte,
            chunk.end_byte,
            &source,
        )?;
        source_bytes = source_bytes
            .checked_add(length)
            .ok_or(ClientError::InvalidResponseCorrelation)?;
        text_bytes = text_bytes
            .checked_add(
                u64::try_from(chunk.content.len())
                    .map_err(|_| ClientError::InvalidResponseCorrelation)?,
            )
            .ok_or(ClientError::InvalidResponseCorrelation)?;
        let extracted_from = parse_extracted_from(chunk.extracted_from, source.file, &context)?;
        chunks.push(SourceTextChunk {
            extracted_from,
            source,
            path: chunk.path,
            start_byte: chunk.start_byte,
            end_byte: chunk.end_byte,
            start_line,
            end_line,
            content: chunk.content,
            physical_encoding,
            content_hash,
            language: chunk.language,
            tier: parse_analysis_tier(chunk.tier)?,
            generated: chunk.generated,
        });
    }
    if source_bytes != response.total_source_bytes
        || source_bytes != context.usage.source_bytes
        || text_bytes != response.total_text_bytes
        || context.usage.results
            != u64::try_from(chunks.len()).map_err(|_| ClientError::InvalidResponseCorrelation)?
    {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    let conditional_activity = parse_conditional_activity(
        response.conditional_activity,
        &context,
        chunks
            .iter()
            .enumerate()
            .filter(|(_, chunk)| conditional_language(&chunk.language))
            .map(|(index, chunk)| ConditionalActivitySubject {
                index,
                basis: ConditionalActivityBasis::ReturnedRange,
                source: Some(&chunk.source),
                start_byte: chunk.start_byte,
                end_byte: chunk.end_byte,
            }),
        chunks.len(),
        32,
        selected_protocol_minor,
        &execution_completeness,
        false,
    )?;
    let source_recovery = source_recovery::parse_source_recovery(
        response.source_recovery,
        chunks.iter().map(|chunk| chunk.source.file),
        selected_protocol_minor,
    )?;
    source_recovery::validate_recovery_coverage(
        &source_recovery,
        &context,
        chunks.iter().map(|chunk| chunk.tier),
    )?;
    Ok(SourceTextReadResult {
        context,
        chunks,
        source_recovery,
        total_source_bytes: response.total_source_bytes,
        total_text_bytes: response.total_text_bytes,
        truncated: response.truncated,
        execution_completeness,
        conditional_activity,
    })
}

fn parse_source_text_prefix_read_for_minor(
    response: daemon::SourceReadResponse,
    repository: RepositoryId,
    selector: GenerationSelector,
    requested: &[SourceReference],
    projection: SourceTextReadOptions,
    maximum_text_bytes_per_chunk: u32,
    selected_protocol_minor: u32,
) -> Result<SourceTextPrefixReadResult, ClientError> {
    if selected_protocol_minor < rootlight_protocol::LOSSLESS_SOURCE_TEXT_PREFIX_PROTOCOL_MINOR {
        return Err(ClientError::ProtocolFeatureUnavailable);
    }
    if maximum_text_bytes_per_chunk == 0
        || u64::from(maximum_text_bytes_per_chunk)
            > rootlight_protocol::MAX_FIRST_SLICE_BUDGET_SOURCE_BYTES
        || projection.merge_overlaps
        || projection.include_line_numbers
    {
        return Err(ClientError::InvalidFirstSliceRequest);
    }
    require_first_slice_response_schema(response.schema_version)?;
    let prefix_truncated = response
        .prefix_truncated
        .ok_or(ClientError::InvalidResponseCorrelation)?;
    let execution_completeness =
        parse_result_completeness(response.completeness, Some(response.truncated), false)?;
    let context = parse_query_context(response.context, repository, selector)?;
    if requested.is_empty()
        || requested.len() > 32
        || context.generation
            != requested
                .first()
                .ok_or(ClientError::InvalidResponseCorrelation)?
                .generation
        || response.chunks.len() > requested.len()
        || (!response.truncated && response.chunks.len() != requested.len())
    {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    let mut chunks = Vec::new();
    chunks
        .try_reserve_exact(response.chunks.len())
        .map_err(|_| ClientError::ResponseAllocationFailed)?;
    let mut source_bytes = 0_u64;
    let mut text_bytes = 0_u64;
    let mut approved_range_clipped = false;
    for (chunk, expected) in response.chunks.into_iter().zip(requested) {
        let source = parse_source_reference(
            chunk
                .source
                .ok_or(ClientError::InvalidResponseCorrelation)?,
            &context,
        )?;
        let content_hash = parse_content_hash(chunk.content_hash)?;
        let length = chunk
            .end_byte
            .checked_sub(chunk.start_byte)
            .ok_or(ClientError::InvalidResponseCorrelation)?;
        if &source != expected
            || chunk.start_byte > source.start_byte
            || projection.context_lines_before == 0 && chunk.start_byte != source.start_byte
            || projection.context_lines_after == 0 && chunk.end_byte > source.end_byte
            || chunk.included_start_line.is_some()
            || chunk.included_end_line.is_some()
            || chunk.start_line != 0
            || chunk.end_line != 0
            || content_hash != source.content_hash
            || chunk.encoding != daemon::SourceReadEncoding::LosslessText as i32
            || !chunk.exact_content.is_empty()
            || u64::try_from(chunk.content.len())
                .map_err(|_| ClientError::InvalidResponseCorrelation)?
                > u64::from(maximum_text_bytes_per_chunk)
        {
            return Err(ClientError::InvalidResponseCorrelation);
        }
        approved_range_clipped |= chunk.end_byte < source.end_byte;
        let physical_encoding = parse_source_text_physical_encoding(chunk.physical_encoding)?;
        validate_prefix_text_coordinates(
            &chunk.content,
            physical_encoding,
            chunk.start_byte,
            chunk.end_byte,
            &source,
        )?;
        source_bytes = source_bytes
            .checked_add(length)
            .ok_or(ClientError::InvalidResponseCorrelation)?;
        text_bytes = text_bytes
            .checked_add(
                u64::try_from(chunk.content.len())
                    .map_err(|_| ClientError::InvalidResponseCorrelation)?,
            )
            .ok_or(ClientError::InvalidResponseCorrelation)?;
        let extracted_from = parse_extracted_from(chunk.extracted_from, source.file, &context)?;
        chunks.push(SourceTextChunk {
            extracted_from,
            source,
            path: chunk.path,
            start_byte: chunk.start_byte,
            end_byte: chunk.end_byte,
            start_line: None,
            end_line: None,
            content: chunk.content,
            physical_encoding,
            content_hash,
            language: chunk.language,
            tier: parse_analysis_tier(chunk.tier)?,
            generated: chunk.generated,
        });
    }
    validate_prefix_flag(prefix_truncated, approved_range_clipped, projection)?;
    if source_bytes != response.total_source_bytes
        || source_bytes != context.usage.source_bytes
        || text_bytes != response.total_text_bytes
        || context.usage.results
            != u64::try_from(chunks.len()).map_err(|_| ClientError::InvalidResponseCorrelation)?
    {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    let conditional_activity = parse_conditional_activity(
        response.conditional_activity,
        &context,
        chunks
            .iter()
            .enumerate()
            .filter(|(_, chunk)| conditional_language(&chunk.language))
            .map(|(index, chunk)| ConditionalActivitySubject {
                index,
                basis: ConditionalActivityBasis::ReturnedRange,
                source: Some(&chunk.source),
                start_byte: chunk.start_byte,
                end_byte: chunk.end_byte,
            }),
        chunks.len(),
        32,
        selected_protocol_minor,
        &execution_completeness,
        false,
    )?;
    let source_recovery = source_recovery::parse_source_recovery(
        response.source_recovery,
        chunks.iter().map(|chunk| chunk.source.file),
        selected_protocol_minor,
    )?;
    source_recovery::validate_recovery_coverage(
        &source_recovery,
        &context,
        chunks.iter().map(|chunk| chunk.tier),
    )?;
    Ok(SourceTextPrefixReadResult {
        result: SourceTextReadResult {
            context,
            chunks,
            source_recovery,
            total_source_bytes: response.total_source_bytes,
            total_text_bytes: response.total_text_bytes,
            truncated: response.truncated,
            execution_completeness,
            conditional_activity,
        },
        prefix_truncated,
    })
}

fn validate_prefix_flag(
    prefix_truncated: bool,
    approved_range_clipped: bool,
    projection: SourceTextReadOptions,
) -> Result<(), ClientError> {
    // The omitted suffix-context endpoint is not observable in a partial response.
    if approved_range_clipped && !prefix_truncated
        || projection.context_lines_after == 0 && prefix_truncated != approved_range_clipped
    {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    Ok(())
}

fn parse_source_text_physical_encoding(
    encoding: Option<i32>,
) -> Result<SourceTextEncoding, ClientError> {
    match encoding.and_then(|encoding| daemon::SourcePhysicalEncoding::try_from(encoding).ok()) {
        Some(daemon::SourcePhysicalEncoding::Utf8) => Ok(SourceTextEncoding::Utf8),
        Some(daemon::SourcePhysicalEncoding::Utf16Le) => Ok(SourceTextEncoding::Utf16Le),
        Some(daemon::SourcePhysicalEncoding::Utf16Be) => Ok(SourceTextEncoding::Utf16Be),
        None => Err(ClientError::InvalidResponseCorrelation),
    }
}

fn validate_prefix_text_coordinates(
    text: &str,
    encoding: SourceTextEncoding,
    start_byte: u64,
    end_byte: u64,
    source: &SourceReference,
) -> Result<(), ClientError> {
    let utf16 = encoding != SourceTextEncoding::Utf8;
    if utf16
        && (text.contains('\0')
            || start_byte == 0 && !text.is_empty() && !text.starts_with('\u{feff}')
            || [start_byte, end_byte, source.start_byte, source.end_byte]
                .into_iter()
                .any(|offset| offset % 2 != 0))
    {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    let mut cursor = start_byte;
    // Endpoints outside the returned prefix remain the source service's responsibility.
    let mut selected_start = source.start_byte > end_byte || source.start_byte == cursor;
    let mut selected_end = source.end_byte > end_byte || source.end_byte == cursor;
    for character in text.chars() {
        let width = if utf16 {
            character.len_utf16() * 2
        } else {
            character.len_utf8()
        };
        cursor = cursor
            .checked_add(u64::try_from(width).map_err(|_| ClientError::InvalidResponseCorrelation)?)
            .ok_or(ClientError::InvalidResponseCorrelation)?;
        if cursor > end_byte {
            return Err(ClientError::InvalidResponseCorrelation);
        }
        selected_start |= source.start_byte == cursor;
        selected_end |= source.end_byte == cursor;
    }
    if cursor != end_byte || !selected_start || !selected_end {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    Ok(())
}

fn validate_text_coordinates(
    text: &str,
    encoding: SourceTextEncoding,
    start_byte: u64,
    end_byte: u64,
    source: &SourceReference,
) -> Result<(), ClientError> {
    let utf16 = encoding != SourceTextEncoding::Utf8;
    if utf16 && text.contains('\0') {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    if utf16 && start_byte == 0 && !text.is_empty() && !text.starts_with('\u{feff}') {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    if utf16
        && [start_byte, end_byte, source.start_byte, source.end_byte]
            .into_iter()
            .any(|offset| offset % 2 != 0)
    {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    let mut cursor = start_byte;
    let mut selected_start = source.start_byte == cursor;
    let mut selected_end = source.end_byte == cursor;
    // Scalar endpoints, not just even UTF-16 positions, protect surrogate pairs.
    for character in text.chars() {
        let width = if utf16 {
            character.len_utf16() * 2
        } else {
            character.len_utf8()
        };
        cursor = cursor
            .checked_add(u64::try_from(width).map_err(|_| ClientError::InvalidResponseCorrelation)?)
            .ok_or(ClientError::InvalidResponseCorrelation)?;
        if cursor > end_byte {
            return Err(ClientError::InvalidResponseCorrelation);
        }
        selected_start |= source.start_byte == cursor;
        selected_end |= source.end_byte == cursor;
    }
    if cursor != end_byte || !selected_start || !selected_end {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    Ok(())
}
