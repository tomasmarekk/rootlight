//! Exact source matching keeps its own scan coverage and source-free wire result.

use rootlight_service::{
    FirstSliceSourceFindError, LiteralSearchError, SourceError, SourceFindBudget, SourceFindCursor,
    SourceFindError, SourceFindStop,
};

use super::*;

pub(super) fn require_protocol(minor: u32) -> Result<(), PublicError> {
    if minor < rootlight_protocol::SOURCE_FIND_PROTOCOL_MINOR {
        return Err(unsupported_capability());
    }
    Ok(())
}

pub(super) fn execute(
    service: &FirstSliceService,
    request: daemon::SourceFindRequest,
    context: &FirstSliceIpcContext,
) -> Result<daemon::SourceFindResponse, PublicError> {
    require_protocol(context.selected_protocol_minor)?;
    context
        .cancellation
        .check()
        .map_err(|error| cancellation_error(error.reason()))?;
    if request.literal.is_empty()
        || request.literal.len() > rootlight_protocol::MAX_SOURCE_FIND_LITERAL_BYTES
        || request.maximum_scan_bytes == 0
        || request.maximum_scan_bytes > rootlight_protocol::MAX_SOURCE_FIND_SCAN_BYTES
        || request.maximum_matches == 0
        || request.maximum_matches > rootlight_protocol::MAX_SOURCE_FIND_MATCHES
    {
        return Err(invalid_argument());
    }
    let repository = parse_repository(request.repository.as_ref())?;
    let selected = parse_generation_selector(request.generation.as_ref())?;
    if request.cursor.is_some() && selected.is_none() {
        return Err(invalid_cursor());
    }
    let file = parse_file(request.file.as_ref())?;
    let generation = service
        .resolve_generation(repository, selected)
        .map_err(service_error)?;
    let cursor = request
        .cursor
        .as_ref()
        .map(|cursor| {
            Ok::<_, PublicError>(SourceFindCursor {
                next_byte: usize::try_from(cursor.next_byte).map_err(|_| invalid_cursor())?,
                matched_prefix: usize::try_from(cursor.matched_prefix)
                    .map_err(|_| invalid_cursor())?,
            })
        })
        .transpose()?;
    let (budget, result_cap) = effective_budget(&request, context)?;
    let result = service
        .find_literal(
            generation.generation,
            file,
            &request.literal,
            cursor,
            budget,
            &context.cancellation,
        )
        .map_err(find_error)?;
    context
        .cancellation
        .check()
        .map_err(|error| cancellation_error(error.reason()))?;
    let stop = match result.stop {
        SourceFindStop::EndOfSource => daemon::SourceFindStop::EndOfSource,
        SourceFindStop::ScanLimit => daemon::SourceFindStop::ScanLimit,
        SourceFindStop::MatchLimit if result_cap < request.maximum_matches => {
            daemon::SourceFindStop::ResultLimit
        }
        SourceFindStop::MatchLimit => daemon::SourceFindStop::MatchLimit,
        SourceFindStop::MatcherMemoryLimit => daemon::SourceFindStop::MatcherMemoryLimit,
        SourceFindStop::ResponseMemoryLimit => daemon::SourceFindStop::ResponseMemoryLimit,
    };
    let file = result.file;
    Ok(daemon::SourceFindResponse {
        schema_version: Some(schema_version()),
        repository: Some(repository_to_wire(repository)),
        generation: Some(generation_to_wire(result.generation)),
        file: Some(daemon::SourceFindFile {
            file: Some(file_to_wire(file.id)),
            path: file.path,
            language: file.language,
            content_hash: Some(content_hash_to_wire(file.content_hash)),
            byte_length: file.byte_length,
            encoding: match file.encoding {
                SourceTextEncoding::Utf8 => daemon::SourcePhysicalEncoding::Utf8,
                SourceTextEncoding::Utf16Le => daemon::SourcePhysicalEncoding::Utf16Le,
                SourceTextEncoding::Utf16Be => daemon::SourcePhysicalEncoding::Utf16Be,
            } as i32,
            generated: file.generated,
            extracted_from: file.extracted_from.as_ref().map(source_ref_to_wire),
        }),
        matches: result.matches.iter().map(source_ref_to_wire).collect(),
        scanned_start_byte: result.scanned.start,
        scanned_end_byte: result.scanned.end,
        restoration_bytes: u64::try_from(result.restoration_bytes).map_err(|_| internal_error())?,
        stop: stop as i32,
        continuation: result
            .continuation
            .map(|cursor| {
                Ok::<_, PublicError>(daemon::SourceFindCursor {
                    next_byte: u64::try_from(cursor.next_byte).map_err(|_| internal_error())?,
                    matched_prefix: u64::try_from(cursor.matched_prefix)
                        .map_err(|_| internal_error())?,
                })
            })
            .transpose()?,
        total_metadata_bytes: u64::try_from(result.total_metadata_bytes)
            .map_err(|_| internal_error())?,
        total_response_memory_bytes: u64::try_from(result.total_response_memory_bytes)
            .map_err(|_| internal_error())?,
    })
}

fn effective_budget(
    request: &daemon::SourceFindRequest,
    context: &FirstSliceIpcContext,
) -> Result<(SourceFindBudget, u32), PublicError> {
    let mut budget = SourceFindBudget::new();
    budget.source = source_service_budget(context).source();
    budget.matcher.max_scan_bytes =
        usize::try_from(request.maximum_scan_bytes).map_err(|_| invalid_argument())?;
    let mut result_cap = request.maximum_matches;
    if let Some(effective) = context.effective_budget {
        let cap = u64::from(result_cap)
            .min(effective.results())
            .min(effective.rows().saturating_sub(1));
        if cap == 0 {
            return Err(budget_exceeded());
        }
        result_cap = u32::try_from(cap).map_err(|_| internal_error())?;
        let memory = usize::try_from(effective.memory_bytes()).map_err(|_| budget_exceeded())?;
        // Partition the caller's owned-memory allowance between matcher and
        // response capacity; neither component may recreate broader defaults.
        let response = budget.source.max_response_memory_bytes.min(memory / 2);
        budget.source.max_response_memory_bytes = response;
        budget.source.max_metadata_bytes = budget.source.max_metadata_bytes.min(response);
        budget.matcher.max_memory_bytes = budget
            .matcher
            .max_memory_bytes
            .min(memory.saturating_sub(response));
    }
    budget.matcher.max_matches = usize::try_from(result_cap).map_err(|_| invalid_argument())?;
    budget.validate().map_err(|_| budget_exceeded())?;
    Ok((budget, result_cap))
}

fn find_error(error: FirstSliceSourceFindError) -> PublicError {
    match error {
        FirstSliceSourceFindError::Service(error) => service_error(error),
        FirstSliceSourceFindError::Find(SourceFindError::Literal(error)) => match error {
            LiteralSearchError::EmptyQuery
            | LiteralSearchError::QueryLimit
            | LiteralSearchError::InvalidBudget => invalid_argument(),
            LiteralSearchError::InvalidCursor => invalid_cursor(),
            LiteralSearchError::Cancelled(error) => cancellation_error(error.reason()),
            LiteralSearchError::MemoryLimit
            | LiteralSearchError::RestoreLimit
            | LiteralSearchError::NoProgress => budget_exceeded(),
            LiteralSearchError::AllocationFailed => resource_exhausted(),
            LiteralSearchError::ExternalControlStopped => internal_error(),
        },
        FirstSliceSourceFindError::Find(SourceFindError::Source(error)) => match error {
            SourceError::FileNotFound => not_found(),
            SourceError::GenerationMismatch
            | SourceError::RepositoryMismatch
            | SourceError::StaleSource => stale_generation(),
            SourceError::EncodingUnsupported => {
                service_error(FirstSliceError::SourceEncodingUnsupported)
            }
            SourceError::Cancelled(reason) => cancellation_error(reason),
            SourceError::MemoryUnavailable => resource_exhausted(),
            SourceError::InvalidBudget
            | SourceError::InvalidSourceSpan
            | SourceError::SelectorLimit
            | SourceError::ContextLimit => invalid_argument(),
            SourceError::SnapshotBudgetExceeded
            | SourceError::VerificationBudgetExceeded
            | SourceError::SourceBudgetExceeded
            | SourceError::MetadataStringLimitExceeded
            | SourceError::MetadataBudgetExceeded
            | SourceError::ResponseMemoryBudgetExceeded => budget_exceeded(),
            _ => internal_error(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(reference: &SourceRef, literal: &str) -> daemon::SourceFindRequest {
        daemon::SourceFindRequest {
            schema_version: Some(schema_version()),
            repository: Some(repository_to_wire(reference.repository())),
            generation: Some(daemon::GenerationSelector {
                selector: Some(daemon::generation_selector::Selector::Generation(
                    generation_to_wire(reference.generation()),
                )),
            }),
            file: Some(file_to_wire(reference.span().file())),
            literal: literal.to_owned(),
            maximum_scan_bytes: rootlight_protocol::MAX_SOURCE_FIND_SCAN_BYTES,
            maximum_matches: rootlight_protocol::DEFAULT_SOURCE_FIND_MATCHES,
            cursor: None,
        }
    }

    #[test]
    fn old_protocol_is_rejected_before_repository_access() {
        let service = FirstSliceService::new(2).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let context = FirstSliceIpcContext {
            client_instance_id: ClientInstanceId::SYSTEM,
            selected_protocol_minor: 26,
            cancellation: Cancellation::with_deadline(deadline),
            deadline: Some(deadline),
            effective_budget: None,
            index_admission: None,
        };
        assert_eq!(
            execute(&service, daemon::SourceFindRequest::default(), &context)
                .unwrap_err()
                .code(),
            ErrorCode::UnsupportedCapability
        );
    }

    #[test]
    fn matches_keep_exact_physical_spans_in_all_encodings() {
        let text = "// aaa\u{1f642}aa\r\npub fn lantern() {}\r\n";
        for (raw, encoding, expected) in [
            (
                text.as_bytes().to_vec(),
                daemon::SourcePhysicalEncoding::Utf8,
                vec![(3, 5), (4, 6), (10, 12)],
            ),
            (
                std::iter::once(0xfeff)
                    .chain(text.encode_utf16())
                    .flat_map(u16::to_le_bytes)
                    .collect(),
                daemon::SourcePhysicalEncoding::Utf16Le,
                vec![(8, 12), (10, 14), (18, 22)],
            ),
            (
                std::iter::once(0xfeff)
                    .chain(text.encode_utf16())
                    .flat_map(u16::to_be_bytes)
                    .collect(),
                daemon::SourcePhysicalEncoding::Utf16Be,
                vec![(8, 12), (10, 14), (18, 22)],
            ),
        ] {
            let (service, _fixture, reference, mut context) =
                super::super::tests::indexed_lossless_source(&raw);
            context.selected_protocol_minor = rootlight_protocol::SOURCE_FIND_PROTOCOL_MINOR;
            let result = execute(&service, request(&reference, "aa"), &context).unwrap();
            assert_eq!(result.file.as_ref().unwrap().encoding, encoding as i32);
            assert_eq!(
                result
                    .matches
                    .iter()
                    .map(|hit| (hit.start_byte, hit.end_byte))
                    .collect::<Vec<_>>(),
                expected
            );
            assert!(result.matches.iter().all(
                |hit| hit.content_hash == Some(content_hash_to_wire(reference.content_hash()))
            ));
            assert_eq!(result.scanned_end_byte, u64::try_from(raw.len()).unwrap());
            assert_eq!(result.stop, daemon::SourceFindStop::EndOfSource as i32);
            assert!(result.continuation.is_none());
        }
    }

    #[test]
    fn scan_pages_preserve_internal_literals_and_reject_active_resume() {
        let raw = b"// identifier_long_internal_suffix\npub fn lantern() {}\n";
        let (service, _fixture, reference, mut context) =
            super::super::tests::indexed_lossless_source(raw);
        context.selected_protocol_minor = rootlight_protocol::SOURCE_FIND_PROTOCOL_MINOR;
        let mut input = request(&reference, "long_internal");
        input.maximum_scan_bytes = 1;
        let mut hits = Vec::new();
        loop {
            let result = execute(&service, input.clone(), &context).unwrap();
            hits.extend(
                result
                    .matches
                    .iter()
                    .map(|hit| (hit.start_byte, hit.end_byte)),
            );
            let Some(cursor) = result.continuation else {
                break;
            };
            assert!(cursor.next_byte > input.cursor.as_ref().map_or(0, |cursor| cursor.next_byte));
            input.cursor = Some(cursor);
        }
        assert_eq!(hits, [(14, 27)]);
        input.cursor = Some(daemon::SourceFindCursor {
            next_byte: 1,
            matched_prefix: 0,
        });
        input.generation = Some(daemon::GenerationSelector {
            selector: Some(daemon::generation_selector::Selector::Active(true)),
        });
        assert_eq!(
            execute(&service, input, &context).unwrap_err().code(),
            ErrorCode::InvalidCursor
        );
    }

    #[test]
    fn precise_errors_never_echo_literal_or_source() {
        for (error, code) in [
            (
                SourceFindError::Literal(LiteralSearchError::EmptyQuery),
                ErrorCode::InvalidArgument,
            ),
            (
                SourceFindError::Literal(LiteralSearchError::InvalidCursor),
                ErrorCode::InvalidCursor,
            ),
            (
                SourceFindError::Literal(LiteralSearchError::NoProgress),
                ErrorCode::BudgetExceeded,
            ),
            (
                SourceFindError::Source(SourceError::FileNotFound),
                ErrorCode::NotFound,
            ),
            (
                SourceFindError::Source(SourceError::EncodingUnsupported),
                ErrorCode::UnsupportedCapability,
            ),
        ] {
            let mapped = find_error(FirstSliceSourceFindError::Find(error));
            assert_eq!(mapped.code(), code);
            assert!(!mapped.message().contains("identifier_long_internal_suffix"));
        }
    }

    #[test]
    fn unsupported_source_encoding_retains_precise_admission_details() {
        let error = find_error(FirstSliceSourceFindError::Find(SourceFindError::Source(
            SourceError::EncodingUnsupported,
        )));
        assert_eq!(error.code(), ErrorCode::UnsupportedCapability);
        assert!(!error.retryable());
        assert_eq!(
            error.details().get(&static_detail_key("failure_family")),
            Some(&PublicValue::Label(static_safe_label(
                "unsupported_encoding"
            ))),
        );
        assert_eq!(
            error.details().get(&static_detail_key("failure_stage")),
            Some(&PublicValue::Label(static_safe_label("admission"))),
        );
        assert_eq!(error.details().len(), 2);
        assert_eq!(
            error.message(),
            "source encoding does not support text queries"
        );
    }
}
