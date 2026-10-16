//! A known-file literal scan exposes exact matches and authenticated unfinished work.

use rootlight_ids::FileId;
use rootlight_mcp_contract::source_find::{
    SourceFindCursorBinding, SourceFindCursorBudgets, SourceFindCursorPosition, SourceFindData,
    SourceFindEnvelope, SourceFindFile, SourceFindInput, SourceFindRequestBinding,
    SourceFindSchemaVersion, SourceFindStop,
};
use rootlight_mcp_contract::source_text::SourceTextEncoding;

use super::*;

/// Exact source-only daemon request; literal contents are excluded from Debug.
#[derive(Clone)]
pub struct SourceFindPortRequest {
    repository: RepositoryId,
    generation: client::GenerationSelector,
    file: FileId,
    literal: String,
    search: client::SourceFindOptions,
}

impl SourceFindPortRequest {
    /// Returns the immutable source repository.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }
    /// Returns the explicit immutable generation used for this scan.
    #[must_use]
    pub const fn generation(&self) -> client::GenerationSelector {
        self.generation
    }
    /// Returns the indexed file identity, never a caller filesystem path.
    #[must_use]
    pub const fn file(&self) -> FileId {
        self.file
    }
    /// Returns the exact case-sensitive scalar literal.
    #[must_use]
    pub fn literal(&self) -> &str {
        &self.literal
    }
    /// Returns independently bounded forward work and match controls.
    #[must_use]
    pub const fn search(&self) -> client::SourceFindOptions {
        self.search
    }
}

impl fmt::Debug for SourceFindPortRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceFindPortRequest")
            .field("file", &self.file)
            .field("literal_bytes", &self.literal.len())
            .field("search", &self.search)
            .finish_non_exhaustive()
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "public cursor authentication context is explicit at this narrow boundary"
)]
pub(super) async fn execute<P: FirstSliceClientPort>(
    port: Arc<P>,
    arguments: Map<String, Value>,
    exposure_profile: ExposureProfile,
    cancellation: RequestCancellation,
    unsupported: &PublicError,
    invalid_arguments: &PublicError,
    invalid_cursor: &PublicError,
    cursor_key: CursorSigningKey,
) -> Result<Map<String, Value>, ToolExecutionError> {
    let input: SourceFindInput =
        decode_input(arguments).map_err(|_| ToolExecutionError::new(invalid_arguments.clone()))?;
    input
        .validate()
        .map_err(|_| ToolExecutionError::new(invalid_arguments.clone()))?;
    let repository = repository_id(input.repository.clone(), unsupported)?;
    let profile = input.response_profile.unwrap_or(ResponseProfile::Compact);
    let budget = AnalyticalBudget::new_source(input.budget.as_ref())?;
    let controls = cursor_budgets(&input, budget)?;
    let selected = client_generation(input.generation.clone());
    // Authenticate request scope before even asking the port for generation
    // metadata. The signed file fingerprint is compared with the later result.
    let resumed = match (&input.cursor, selected) {
        (Some(cursor), client::GenerationSelector::Generation(generation)) => Some(
            SourceFindRequestBinding {
                repository,
                generation,
                file_id: input.file_id,
                literal: &input.literal,
                budgets: &controls,
                response_profile: profile,
                exposure_profile,
                snapshot_id: repository_snapshot_id(repository, generation),
                key_id: cursor_key.key_id,
            }
            .resume(cursor, now_unix_ms(), &cursor_key.secret)
            .map_err(|_| ToolExecutionError::new(invalid_cursor.clone()))?,
        ),
        (Some(_), _) => return Err(ToolExecutionError::new(invalid_cursor.clone())),
        (None, _) => None,
    };
    let started_at = prepare_port(&port, &cancellation).await?;
    let status = await_port(
        port.repository_status(
            RepositoryStatusPortRequest::new(repository, selected),
            cancellation.clone(),
        ),
        cancellation.clone(),
    )
    .await?;
    if status.repository_id != repository
        || !safe_display_name(&status.display_name)
        || matches!(selected, client::GenerationSelector::Generation(generation) if generation != status.resolved_generation)
    {
        return Err(internal(ToolExecutionFailure::InvalidResponse));
    }
    let generation = status.resolved_generation;
    let search = client::SourceFindOptions {
        maximum_scan_bytes: input.effective_maximum_scan_bytes(),
        maximum_matches: u32::from(input.effective_maximum_matches()),
        cursor: resumed.as_ref().map(|cursor| client::SourceFindCursor {
            next_byte: cursor.position.next_byte,
            matched_prefix: cursor.position.matched_prefix,
        }),
    };
    let response = await_port(
        port.source_find(
            SourceFindPortRequest {
                repository,
                generation: client::GenerationSelector::Generation(generation),
                file: input.file_id,
                literal: input.literal.clone(),
                search,
            },
            budget.options,
            cancellation.clone(),
        ),
        cancellation,
    )
    .await?;
    let file = map_file(response.file, repository, generation)?;
    if response.repository != repository
        || response.generation != generation
        || file.id != input.file_id
        || response.total_response_memory_bytes > controls.maximum_response_memory_bytes
        || response.total_metadata_bytes > controls.maximum_metadata_bytes
        || resumed.as_ref().is_some_and(|cursor| {
            file.fingerprint(repository, generation).ok() != Some(cursor.file_fingerprint)
        })
    {
        return Err(internal(ToolExecutionFailure::InvalidResponse));
    }
    let matches = response
        .matches
        .iter()
        .map(client_source_ref)
        .collect::<Result<Vec<_>, _>>()?;
    let stop = match response.stop {
        client::SourceFindStop::EndOfSource => SourceFindStop::EndOfSource,
        client::SourceFindStop::ScanLimit => SourceFindStop::ScanLimit,
        client::SourceFindStop::MatchLimit => SourceFindStop::MatchLimit,
        client::SourceFindStop::MatcherMemoryLimit => SourceFindStop::MatcherMemoryLimit,
        client::SourceFindStop::ResponseMemoryLimit => SourceFindStop::ResponseMemoryLimit,
        client::SourceFindStop::ResultLimit => SourceFindStop::ResultLimit,
    };
    validate_request_result(
        &input,
        &controls,
        &file,
        &matches,
        &response.scanned,
        response.restoration_bytes,
        stop,
        search.cursor,
        response.continuation,
    )?;
    let next_cursor = response
        .continuation
        .map(|cursor| {
            SourceFindCursorBinding {
                repository,
                generation,
                file: &file,
                literal: &input.literal,
                budgets: &controls,
                response_profile: profile,
                exposure_profile,
                snapshot_id: repository_snapshot_id(repository, generation),
                key_id: cursor_key.key_id,
            }
            .issue(
                SourceFindCursorPosition {
                    next_byte: cursor.next_byte,
                    matched_prefix: cursor.matched_prefix,
                },
                now_unix_ms(),
                &cursor_key.secret,
            )
            .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))
        })
        .transpose()?;
    let output = SourceFindEnvelope {
        schema_version: SourceFindSchemaVersion::V1_1,
        repository: ResolvedRepository {
            repository_id: repository,
            display_name: status.display_name,
        },
        generation,
        truncated: stop != SourceFindStop::EndOfSource,
        next_cursor: RequiredNullable(next_cursor),
        usage: UsageSummary {
            rows: u64::try_from(matches.len())
                .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))?
                .saturating_add(1),
            edges: 0,
            source_bytes: 0,
            json_bytes: 0,
            estimated_tokens: 0,
            wall_time_ms: 0,
            cache_status: CacheStatus::NotApplicable,
            trace_id: "source-find".to_owned(),
        },
        data: SourceFindData {
            file,
            matches,
            scanned_start_byte: response.scanned.start,
            scanned_end_byte: response.scanned.end,
            restoration_bytes: response.restoration_bytes,
            stop,
            total_metadata_bytes: response.total_metadata_bytes,
            total_response_memory_bytes: response.total_response_memory_bytes,
        },
        trust: TrustClassification::UntrustedRepositoryData,
    };
    output
        .validate()
        .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))?;
    serialize_measured_source_success(output, started_at, budget, |output| &mut output.usage)
}

fn cursor_budgets(
    input: &SourceFindInput,
    budget: AnalyticalBudget,
) -> Result<SourceFindCursorBudgets, ToolExecutionError> {
    let maximums = budget.limits.maximums();
    let response_memory = (320 * 1024).min(maximums.memory_bytes / 2);
    Ok(SourceFindCursorBudgets {
        maximum_scan_bytes: input.effective_maximum_scan_bytes(),
        maximum_matches: input.effective_maximum_matches(),
        maximum_query_bytes: 64 * 1024,
        maximum_snapshot_bytes: 64 * 1024 * 1024,
        maximum_verified_bytes: u64::MAX,
        maximum_matcher_memory_bytes: (2 * 1024 * 1024)
            .min(maximums.memory_bytes.saturating_sub(response_memory)),
        maximum_restore_bytes: 256 * 1024,
        maximum_response_memory_bytes: response_memory,
        maximum_metadata_bytes: (256 * 1024).min(response_memory),
        maximum_results: u16::try_from(
            maximums
                .results
                .min(maximums.rows.saturating_sub(1))
                .min(1024),
        )
        .map_err(|_| internal(ToolExecutionFailure::Executor))?,
        maximum_json_bytes: maximums.json_bytes,
        maximum_tokens: maximums.tokens,
        timeout_ms: maximums.time_ms,
        source_verification_duration_explicit: budget.source_duration != Some(false),
    })
}

fn map_file(
    file: client::SourceFindFile,
    repository: RepositoryId,
    generation: GenerationId,
) -> Result<SourceFindFile, ToolExecutionError> {
    let extracted_from = file
        .extracted_from
        .as_ref()
        .map(client_source_ref)
        .transpose()?;
    let mapped = SourceFindFile {
        id: file.file,
        path: file.path,
        language: file.language,
        content_hash: file.content_hash,
        byte_length: file.byte_length,
        encoding: match file.encoding {
            client::SourceTextEncoding::Utf8 => SourceTextEncoding::Utf8,
            client::SourceTextEncoding::Utf16Le => SourceTextEncoding::Utf16Le,
            client::SourceTextEncoding::Utf16Be => SourceTextEncoding::Utf16Be,
        },
        generated: file.generated,
        trust: TrustClassification::UntrustedRepositoryData,
        extracted_from,
    };
    mapped
        .fingerprint(repository, generation)
        .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))?;
    Ok(mapped)
}

#[expect(
    clippy::too_many_arguments,
    reason = "independent source work and cursor controls are validated without querying source again"
)]
fn validate_request_result(
    input: &SourceFindInput,
    budgets: &SourceFindCursorBudgets,
    file: &SourceFindFile,
    matches: &[SourceRef],
    scanned: &std::ops::Range<u64>,
    restored: u64,
    stop: SourceFindStop,
    incoming: Option<client::SourceFindCursor>,
    outgoing: Option<client::SourceFindCursor>,
) -> Result<(), ToolExecutionError> {
    let invalid = || internal(ToolExecutionFailure::InvalidResponse);
    let origin = if file.encoding == SourceTextEncoding::Utf8 {
        0
    } else {
        2
    };
    let expected_start = incoming.map_or(origin, |cursor| cursor.next_byte);
    let width = if file.encoding == SourceTextEncoding::Utf8 {
        u64::try_from(input.literal.len()).map_err(|_| invalid())?
    } else {
        u64::try_from(input.literal.encode_utf16().count())
            .map_err(|_| invalid())?
            .checked_mul(2)
            .ok_or_else(invalid)?
    };
    let count = u64::try_from(matches.len()).map_err(|_| invalid())?;
    let prefix_scalars = usize::try_from(incoming.map_or(0, |cursor| cursor.matched_prefix))
        .map_err(|_| invalid())?;
    let prefix_width = input
        .literal
        .chars()
        .take(prefix_scalars)
        .try_fold(0_u64, |bytes, scalar| {
            bytes.checked_add(if file.encoding == SourceTextEncoding::Utf8 {
                u64::try_from(scalar.len_utf8()).ok()?
            } else {
                u64::try_from(scalar.len_utf16()).ok()?.checked_mul(2)?
            })
        })
        .ok_or_else(invalid)?;
    let cap = u64::from(input.effective_maximum_matches()).min(u64::from(budgets.maximum_results));
    if scanned.start != expected_start
        || scanned.end < scanned.start
        || scanned.end - scanned.start > budgets.maximum_scan_bytes
        || restored > budgets.maximum_restore_bytes
        || restored > width.min(scanned.start.saturating_sub(origin))
        || restored < prefix_width
        || (incoming.is_none() && restored != 0)
        || count > cap
        || matches.iter().any(|source| {
            source
                .span()
                .end_byte()
                .checked_sub(source.span().start_byte())
                != Some(width)
                || source.span().start_byte() < scanned.start.saturating_sub(prefix_width)
        })
        || (stop == SourceFindStop::MatchLimit
            && count != u64::from(input.effective_maximum_matches()))
        || (stop == SourceFindStop::ResultLimit
            && (cap >= u64::from(input.effective_maximum_matches()) || count != cap))
        || (stop == SourceFindStop::ScanLimit
            && budgets.maximum_scan_bytes - (scanned.end - scanned.start) >= 4)
        || (matches!(
            stop,
            SourceFindStop::MatchLimit
                | SourceFindStop::ResultLimit
                | SourceFindStop::MatcherMemoryLimit
                | SourceFindStop::ResponseMemoryLimit
        ) && !matches
            .last()
            .is_some_and(|source| source.span().end_byte() == scanned.end))
        || (matches!(
            stop,
            SourceFindStop::ScanLimit
                | SourceFindStop::MatcherMemoryLimit
                | SourceFindStop::ResponseMemoryLimit
        ) && count >= cap)
        || outgoing.is_some_and(|cursor| {
            cursor.next_byte != scanned.end
                || u64::try_from(input.literal.chars().count())
                    .map_or(true, |count| cursor.matched_prefix >= count)
                || prefix_bytes(&input.literal, file.encoding, cursor.matched_prefix)
                    .is_none_or(|bytes| bytes > scanned.end.saturating_sub(origin))
        })
        || outgoing.is_some() == (stop == SourceFindStop::EndOfSource)
    {
        return Err(invalid());
    }
    Ok(())
}

fn prefix_bytes(literal: &str, encoding: SourceTextEncoding, scalars: u64) -> Option<u64> {
    literal
        .chars()
        .take(usize::try_from(scalars).ok()?)
        .try_fold(0_u64, |bytes, scalar| {
            bytes.checked_add(if encoding == SourceTextEncoding::Utf8 {
                u64::try_from(scalar.len_utf8()).ok()?
            } else {
                u64::try_from(scalar.len_utf16()).ok()?.checked_mul(2)?
            })
        })
}
