//! Admission and response correlation for exact literal source search.
//!
//! The transport verifies identity, physical spans and declared work without
//! reading source bodies or attempting to authenticate the matcher's cursor.

use super::{
    FirstSliceEffectiveBudget, FirstSliceIpcRequest, FirstSliceIpcResponse, PublicError, daemon,
    explicit_generation_bytes, first_slice_schema_matches, invalid_argument,
    require_first_slice_schema, require_wire_id, validate_generation_selector,
    validate_source_reference, wire_id_has_len,
};
use rootlight_protocol::{
    MAX_FIRST_SLICE_BUDGET_MEMORY_BYTES, MAX_SOURCE_FIND_LITERAL_BYTES, MAX_SOURCE_FIND_MATCHES,
    MAX_SOURCE_FIND_SCAN_BYTES, SOURCE_FIND_PROTOCOL_MINOR,
};

pub(super) fn validate_request(
    request: &daemon::SourceFindRequest,
) -> Result<(), Box<PublicError>> {
    require_first_slice_schema(request.schema_version.as_ref())?;
    require_wire_id(
        request.repository.as_ref().map(|id| id.value.as_slice()),
        16,
    )?;
    require_wire_id(request.file.as_ref().map(|id| id.value.as_slice()), 20)?;
    validate_generation_selector(request.generation.as_ref())?;
    if request.literal.is_empty()
        || request.literal.len() > MAX_SOURCE_FIND_LITERAL_BYTES
        || !(1..=MAX_SOURCE_FIND_SCAN_BYTES).contains(&request.maximum_scan_bytes)
        || !(1..=MAX_SOURCE_FIND_MATCHES).contains(&request.maximum_matches)
    {
        return Err(Box::new(invalid_argument(
            "literal source search controls are invalid",
        )));
    }
    let scalars = request.literal.chars().count() as u64;
    if request.cursor.as_ref().is_some_and(|cursor| {
        explicit_generation_bytes(request.generation.as_ref()).is_none()
            || cursor.matched_prefix >= scalars
            || cursor.matched_prefix > cursor.next_byte
    }) {
        return Err(Box::new(invalid_argument(
            "literal source search controls are invalid",
        )));
    }
    Ok(())
}

fn prefix_width(literal: &str, utf16: bool, scalars: u64) -> u64 {
    literal
        .chars()
        .take(usize::try_from(scalars).unwrap_or(usize::MAX))
        .map(|character| {
            if utf16 {
                character.len_utf16() as u64 * 2
            } else {
                character.len_utf8() as u64
            }
        })
        .sum()
}

pub(super) fn correlates(
    request: &daemon::SourceFindRequest,
    response: &daemon::SourceFindResponse,
    selected_minor: u32,
) -> bool {
    if selected_minor < SOURCE_FIND_PROTOCOL_MINOR || validate_request(request).is_err() {
        return false;
    }
    let Some(file) = &response.file else {
        return false;
    };
    let Ok(encoding) = daemon::SourcePhysicalEncoding::try_from(file.encoding) else {
        return false;
    };
    let utf16 = encoding != daemon::SourcePhysicalEncoding::Utf8;
    let content_start = if utf16 { 2 } else { 0 };
    let width = if utf16 {
        request.literal.encode_utf16().count() as u64 * 2
    } else {
        request.literal.len() as u64
    };
    let scalars = request.literal.chars().count() as u64;
    let start = request
        .cursor
        .as_ref()
        .map_or(content_start, |cursor| cursor.next_byte);
    let prefix_bytes = request.cursor.as_ref().map_or(0, |cursor| {
        prefix_width(&request.literal, utf16, cursor.matched_prefix)
    });
    let end = response.scanned_end_byte;
    let metadata = file.path.len() as u64 + file.language.len() as u64;
    if !first_slice_schema_matches(response.schema_version.as_ref())
        || response.repository != request.repository
        || !wire_id_has_len(response.generation.as_ref().map(|id| &id.value), 20)
        || explicit_generation_bytes(request.generation.as_ref()).is_some_and(|generation| {
            response
                .generation
                .as_ref()
                .is_none_or(|id| id.value != generation)
        })
        || file.file != request.file
        || !wire_id_has_len(file.content_hash.as_ref().map(|hash| &hash.value), 32)
        || file.path.is_empty()
        || file.path.contains('\0')
        || file.language.contains('\0')
        || file.byte_length < content_start
        || (utf16 && !file.byte_length.is_multiple_of(2))
        || start < content_start
        || response.scanned_start_byte != start
        || end < start
        || end > file.byte_length
        || end - start > request.maximum_scan_bytes
        || (utf16
            && (!start.is_multiple_of(2)
                || !end.is_multiple_of(2)
                || !response.restoration_bytes.is_multiple_of(2)))
        || response.matches.len() > request.maximum_matches as usize
        || response.restoration_bytes > width.min(start - content_start)
        || response.restoration_bytes < prefix_bytes
        || (request.cursor.is_none() && response.restoration_bytes != 0)
        || response.total_metadata_bytes != metadata
        || response.total_response_memory_bytes < metadata
        || response.total_response_memory_bytes > MAX_FIRST_SLICE_BUDGET_MEMORY_BYTES
    {
        return false;
    }
    if let Some(origin) = &file.extracted_from
        && (validate_source_reference(origin).is_err()
            || origin.repository != response.repository
            || origin.generation != response.generation
            || origin.file == file.file
            || origin.start_byte != 0
            || origin.end_byte == 0)
    {
        return false;
    }
    let mut previous = None;
    for source in &response.matches {
        if validate_source_reference(source).is_err()
            || source.repository != response.repository
            || source.generation != response.generation
            || source.file != file.file
            || source.content_hash != file.content_hash
            || source.start_byte < content_start.max(start.saturating_sub(prefix_bytes))
            || source.end_byte <= start
            || source.end_byte > end
            || source.end_byte.checked_sub(source.start_byte) != Some(width)
            || (utf16
                && (!source.start_byte.is_multiple_of(2) || !source.end_byte.is_multiple_of(2)))
            || previous.is_some_and(|old| source.start_byte <= old)
        {
            return false;
        }
        previous = Some(source.start_byte);
    }
    let Ok(stop) = daemon::SourceFindStop::try_from(response.stop) else {
        return false;
    };
    if stop == daemon::SourceFindStop::EndOfSource {
        return end == file.byte_length && response.continuation.is_none();
    }
    let Some(cursor) = &response.continuation else {
        return false;
    };
    if end <= start
        || end >= file.byte_length
        || cursor.next_byte != end
        || cursor.matched_prefix >= scalars
        || prefix_width(&request.literal, utf16, cursor.matched_prefix) > end - content_start
    {
        return false;
    }
    match stop {
        daemon::SourceFindStop::ScanLimit => {
            // A page stops before a scalar that would cross the physical-byte cap.
            request.maximum_scan_bytes - (end - start) < 4
        }
        daemon::SourceFindStop::MatchLimit => {
            response.matches.len() == request.maximum_matches as usize
                && response
                    .matches
                    .last()
                    .is_some_and(|source| source.end_byte == end)
        }
        daemon::SourceFindStop::MatcherMemoryLimit
        | daemon::SourceFindStop::ResponseMemoryLimit
        | daemon::SourceFindStop::ResultLimit => response
            .matches
            .last()
            .is_some_and(|source| source.end_byte == end),
        daemon::SourceFindStop::EndOfSource => false,
    }
}

pub(super) fn budget_correlates(
    request: &FirstSliceIpcRequest,
    response: &FirstSliceIpcResponse,
    budget: Option<FirstSliceEffectiveBudget>,
) -> bool {
    let (FirstSliceIpcRequest::SourceFind(request), FirstSliceIpcResponse::SourceFind(response)) =
        (request, response)
    else {
        return true;
    };
    let limit = budget.map_or(u64::from(request.maximum_matches), |budget| {
        u64::from(request.maximum_matches)
            .min(budget.results)
            .min(budget.rows.saturating_sub(1))
    });
    let count = response.matches.len() as u64;
    if limit == 0
        || count > limit
        || budget.is_some_and(|budget| response.total_response_memory_bytes > budget.memory_bytes)
    {
        return false;
    }
    match daemon::SourceFindStop::try_from(response.stop) {
        Ok(daemon::SourceFindStop::ResultLimit) => {
            limit < u64::from(request.maximum_matches) && count == limit
        }
        Ok(daemon::SourceFindStop::MatchLimit) => {
            limit == u64::from(request.maximum_matches) && count == limit
        }
        Ok(
            daemon::SourceFindStop::ScanLimit
            | daemon::SourceFindStop::MatcherMemoryLimit
            | daemon::SourceFindStop::ResponseMemoryLimit,
        ) => count < limit,
        Ok(daemon::SourceFindStop::EndOfSource) => true,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Cancellation, ClientInstanceId, ControlMethod, Duration, FirstSliceIpcContext,
        FirstSliceIpcFuture, FirstSliceIpcHandler, Instant, common, control_method_from_wire,
        dispatch_first_slice, first_slice_request_from_wire,
        first_slice_response_correlates_for_minor,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn fixture() -> (daemon::SourceFindRequest, daemon::SourceFindResponse) {
        let repository = Some(common::RepositoryId { value: vec![1; 16] });
        let generation = Some(common::GenerationId { value: vec![2; 20] });
        let file = Some(common::FileId { value: vec![3; 20] });
        let hash = Some(common::ContentHash { value: vec![4; 32] });
        let schema = Some(common::ContractVersion { major: 1, minor: 0 });
        let request = daemon::SourceFindRequest {
            schema_version: schema,
            repository: repository.clone(),
            generation: Some(daemon::GenerationSelector {
                selector: generation
                    .clone()
                    .map(daemon::generation_selector::Selector::Generation),
            }),
            file: file.clone(),
            literal: "aa".into(),
            cursor: None,
            maximum_scan_bytes: 1024,
            maximum_matches: 128,
        };
        let response = daemon::SourceFindResponse {
            schema_version: schema,
            repository: repository.clone(),
            generation: generation.clone(),
            file: Some(daemon::SourceFindFile {
                file: file.clone(),
                path: "src/a.v".into(),
                language: "v".into(),
                content_hash: hash.clone(),
                byte_length: 4,
                encoding: daemon::SourcePhysicalEncoding::Utf8 as i32,
                generated: false,
                extracted_from: None,
            }),
            matches: (0..3)
                .map(|start| daemon::FirstSliceSourceRef {
                    repository: repository.clone(),
                    generation: generation.clone(),
                    file: file.clone(),
                    content_hash: hash.clone(),
                    start_byte: start,
                    end_byte: start + 2,
                    start_line: None,
                    end_line: None,
                })
                .collect(),
            scanned_start_byte: 0,
            scanned_end_byte: 4,
            restoration_bytes: 0,
            stop: daemon::SourceFindStop::EndOfSource as i32,
            continuation: None,
            total_metadata_bytes: 8,
            total_response_memory_bytes: 1024,
        };
        (request, response)
    }

    #[test]
    fn source_find_admission_keeps_literal_bytes_and_generation_authority() {
        let (request, _) = fixture();
        assert!(validate_request(&request).is_ok());
        for literal in ["", &"x".repeat(MAX_SOURCE_FIND_LITERAL_BYTES + 1)] {
            let mut changed = request.clone();
            changed.literal = literal.into();
            let error = validate_request(&changed).unwrap_err();
            assert!(!format!("{error:?}").contains("src/a.v"));
        }
        let mut controls = vec![];
        let mut changed = request.clone();
        changed.maximum_matches = 0;
        controls.push(changed);
        let mut changed = request.clone();
        changed.maximum_scan_bytes = 0;
        controls.push(changed);
        let mut changed = request.clone();
        changed.maximum_scan_bytes = MAX_SOURCE_FIND_SCAN_BYTES + 1;
        controls.push(changed);
        let mut changed = request.clone();
        changed.maximum_matches = MAX_SOURCE_FIND_MATCHES + 1;
        controls.push(changed);
        let mut changed = request.clone();
        changed.file.as_mut().unwrap().value.pop();
        controls.push(changed);
        let mut changed = request.clone();
        changed.cursor = Some(daemon::SourceFindCursor {
            next_byte: 10,
            matched_prefix: 2,
        });
        controls.push(changed);
        for changed in controls {
            assert!(validate_request(&changed).is_err());
        }
        let mut active = request;
        active.generation = Some(daemon::GenerationSelector {
            selector: Some(daemon::generation_selector::Selector::Active(true)),
        });
        active.literal = "\0☃".into();
        assert!(
            validate_request(&active).is_ok(),
            "literal NUL is source data"
        );
        active.cursor = Some(daemon::SourceFindCursor {
            next_byte: 4,
            matched_prefix: 0,
        });
        assert!(
            validate_request(&active).is_err(),
            "continuation needs a pinned generation"
        );
    }

    #[test]
    fn source_find_correlation_rejects_forged_identity_spans_and_totals() {
        let (request, response) = fixture();
        assert!(correlates(&request, &response, 27));
        assert!(!correlates(&request, &response, 26));
        let mut changes = vec![];
        let mut r = response.clone();
        r.repository.as_mut().unwrap().value[0] = 9;
        changes.push(r);
        let mut r = response.clone();
        r.generation.as_mut().unwrap().value[0] = 9;
        changes.push(r);
        let mut r = response.clone();
        r.file.as_mut().unwrap().file.as_mut().unwrap().value[0] = 9;
        changes.push(r);
        let mut r = response.clone();
        r.file.as_mut().unwrap().content_hash = None;
        changes.push(r);
        let mut r = response.clone();
        r.matches[0].content_hash.as_mut().unwrap().value[0] = 9;
        changes.push(r);
        let mut r = response.clone();
        r.matches[0].end_byte += 1;
        changes.push(r);
        let mut r = response.clone();
        r.matches.reverse();
        changes.push(r);
        let mut r = response.clone();
        r.matches.push(r.matches[2].clone());
        changes.push(r);
        let mut r = response.clone();
        r.scanned_end_byte = 3;
        changes.push(r);
        let mut r = response.clone();
        r.scanned_start_byte = 1;
        changes.push(r);
        let mut r = response.clone();
        r.restoration_bytes = 1;
        changes.push(r);
        let mut r = response.clone();
        r.total_metadata_bytes += 1;
        changes.push(r);
        let mut r = response.clone();
        r.total_response_memory_bytes = 7;
        changes.push(r);
        let mut r = response.clone();
        r.file.as_mut().unwrap().encoding = 99;
        changes.push(r);
        let mut r = response.clone();
        r.stop = 99;
        changes.push(r);
        let mut r = response.clone();
        r.continuation = Some(daemon::SourceFindCursor {
            next_byte: 4,
            matched_prefix: 1,
        });
        changes.push(r);
        for (index, changed) in changes.iter().enumerate() {
            assert!(
                !correlates(&request, changed, 27),
                "forged response {index}"
            );
        }
        let mut extracted = response;
        let mut origin = extracted.matches[0].clone();
        origin.file.as_mut().unwrap().value[0] = 8;
        origin.start_byte = 0;
        origin.end_byte = 20;
        extracted.file.as_mut().unwrap().extracted_from = Some(origin);
        assert!(correlates(&request, &extracted, 27));
        extracted
            .file
            .as_mut()
            .unwrap()
            .extracted_from
            .as_mut()
            .unwrap()
            .generation = None;
        assert!(!correlates(&request, &extracted, 27));
    }

    #[test]
    fn source_find_physical_widths_and_cross_page_matches_are_exact() {
        for encoding in [
            daemon::SourcePhysicalEncoding::Utf8,
            daemon::SourcePhysicalEncoding::Utf16Le,
            daemon::SourcePhysicalEncoding::Utf16Be,
        ] {
            let (mut request, mut response) = fixture();
            request.literal = "☃😀".into();
            let (start, width) = if encoding == daemon::SourcePhysicalEncoding::Utf8 {
                (0, 7)
            } else {
                (2, 6)
            };
            response.file.as_mut().unwrap().encoding = encoding as i32;
            response.file.as_mut().unwrap().byte_length = start + width;
            response.scanned_start_byte = start;
            response.scanned_end_byte = start + width;
            response.matches.truncate(1);
            response.matches[0].start_byte = start;
            response.matches[0].end_byte = start + width;
            assert!(correlates(&request, &response, 27));
            let split = start
                + if encoding == daemon::SourcePhysicalEncoding::Utf8 {
                    3
                } else {
                    2
                };
            request.cursor = Some(daemon::SourceFindCursor {
                next_byte: split,
                matched_prefix: 1,
            });
            response.scanned_start_byte = split;
            response.restoration_bytes = split - start;
            assert!(
                correlates(&request, &response, 27),
                "overlapping prefix crosses the page"
            );
            response.matches[0].start_byte += 1;
            assert!(!correlates(&request, &response, 27));
        }
    }

    #[test]
    fn source_find_partial_stops_preserve_progress_and_effective_caps() {
        let (mut request, mut response) = fixture();
        request.maximum_matches = 1;
        response.matches.truncate(1);
        response.scanned_end_byte = 2;
        response.stop = daemon::SourceFindStop::MatchLimit as i32;
        response.continuation = Some(daemon::SourceFindCursor {
            next_byte: 2,
            matched_prefix: 1,
        });
        assert!(correlates(&request, &response, 27));
        response.continuation.as_mut().unwrap().next_byte = 1;
        assert!(!correlates(&request, &response, 27));
        response.continuation.as_mut().unwrap().next_byte = 2;
        request.maximum_matches = 128;
        response.stop = daemon::SourceFindStop::ResultLimit as i32;
        let req = FirstSliceIpcRequest::SourceFind(request.clone());
        let res = FirstSliceIpcResponse::SourceFind(response.clone());
        let mut budget = FirstSliceEffectiveBudget {
            rows: 10,
            edges: 10,
            results: 1,
            source_bytes: 1,
            json_bytes: 4096,
            estimated_tokens: 4096,
            memory_bytes: 2048,
            duration: Duration::from_secs(1),
            depth: None,
            paths: None,
            source_verification_duration_explicit: true,
        };
        assert!(correlates(&request, &response, 27));
        assert!(budget_correlates(&req, &res, Some(budget)));
        assert!(!budget_correlates(&req, &res, None));
        budget.results = 128;
        budget.rows = 2;
        assert!(budget_correlates(&req, &res, Some(budget)));
        budget.rows = 1;
        assert!(!budget_correlates(&req, &res, Some(budget)));
        budget.rows = 2;
        budget.memory_bytes = 1;
        assert!(!budget_correlates(&req, &res, Some(budget)));
        request.maximum_scan_bytes = 2;
        response.stop = daemon::SourceFindStop::ScanLimit as i32;
        assert!(correlates(&request, &response, 27));
        response.scanned_end_byte = 0;
        assert!(!correlates(&request, &response, 27));
    }

    #[test]
    fn source_find_cursor_prefix_and_stop_claims_bound_cross_page_evidence() {
        let (mut request, mut response) = fixture();
        request.cursor = Some(daemon::SourceFindCursor {
            next_byte: 2,
            matched_prefix: 1,
        });
        request.maximum_scan_bytes = 1;
        response.scanned_start_byte = 2;
        response.scanned_end_byte = 3;
        response.restoration_bytes = 2;
        response.matches = vec![response.matches[1].clone()];
        response.stop = daemon::SourceFindStop::ScanLimit as i32;
        response.continuation = Some(daemon::SourceFindCursor {
            next_byte: 3,
            matched_prefix: 1,
        });
        assert!(correlates(&request, &response, 27));
        request.cursor.as_mut().unwrap().matched_prefix = 0;
        assert!(
            !correlates(&request, &response, 27),
            "match starts before the declared incoming prefix"
        );
        request.cursor.as_mut().unwrap().matched_prefix = 1;
        response.restoration_bytes = 0;
        assert!(
            !correlates(&request, &response, 27),
            "restoration cannot omit the matched prefix"
        );
        response.restoration_bytes = 2;
        let budget = FirstSliceEffectiveBudget {
            rows: 2,
            edges: 1,
            results: 1,
            source_bytes: 1,
            json_bytes: 4096,
            estimated_tokens: 4096,
            memory_bytes: 4096,
            duration: Duration::from_secs(1),
            depth: None,
            paths: None,
            source_verification_duration_explicit: true,
        };
        let req = FirstSliceIpcRequest::SourceFind(request);
        for stop in [
            daemon::SourceFindStop::ScanLimit,
            daemon::SourceFindStop::MatcherMemoryLimit,
            daemon::SourceFindStop::ResponseMemoryLimit,
        ] {
            response.stop = stop as i32;
            let res = FirstSliceIpcResponse::SourceFind(response.clone());
            assert!(
                !budget_correlates(&req, &res, Some(budget)),
                "result cap requires its own stop"
            );
        }
    }

    #[test]
    fn source_find_empty_sources_preserve_eof_without_invented_continuation() {
        for encoding in [
            daemon::SourcePhysicalEncoding::Utf8,
            daemon::SourcePhysicalEncoding::Utf16Le,
        ] {
            let (request, mut response) = fixture();
            let end = if encoding == daemon::SourcePhysicalEncoding::Utf8 {
                0
            } else {
                2
            };
            response.file.as_mut().unwrap().encoding = encoding as i32;
            response.file.as_mut().unwrap().byte_length = end;
            response.matches.clear();
            response.scanned_start_byte = end;
            response.scanned_end_byte = end;
            assert!(correlates(&request, &response, 27));
            response.stop = daemon::SourceFindStop::ScanLimit as i32;
            response.continuation = Some(daemon::SourceFindCursor {
                next_byte: end,
                matched_prefix: 0,
            });
            assert!(!correlates(&request, &response, 27));
        }
    }

    #[derive(Default)]
    struct Handler {
        calls: AtomicUsize,
    }
    impl FirstSliceIpcHandler for Handler {
        fn dispatch(
            &self,
            _request: FirstSliceIpcRequest,
            _context: FirstSliceIpcContext,
        ) -> FirstSliceIpcFuture {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(FirstSliceIpcResponse::SourceFind(fixture().1)) })
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn source_find_wire_dispatch_checks_version_and_handler_output() {
        for minor in [26, 27] {
            let request = fixture().0;
            assert_eq!(
                control_method_from_wire(Some(&daemon::request_envelope::Request::SourceFind(
                    request.clone()
                ))),
                ControlMethod::Unknown
            );
            let request = first_slice_request_from_wire(Some(
                daemon::request_envelope::Request::SourceFind(request),
            ))
            .unwrap()
            .unwrap();
            let handler = Handler::default();
            let response = dispatch_first_slice(
                &handler,
                request,
                FirstSliceIpcContext {
                    client_instance_id: ClientInstanceId::SYSTEM,
                    selected_protocol_minor: minor,
                    cancellation: Cancellation::new(),
                    deadline: Some(Instant::now() + Duration::from_secs(1)),
                    effective_budget: None,
                    index_admission: None,
                },
            )
            .await;
            assert_eq!(
                handler.calls.load(Ordering::SeqCst),
                usize::from(minor == 27)
            );
            assert_eq!(
                matches!(response, daemon::response_envelope::Response::SourceFind(_)),
                minor == 27
            );
        }
        let handler = Handler::default();
        let response = dispatch_first_slice(
            &handler,
            FirstSliceIpcRequest::SourceFind(fixture().0),
            FirstSliceIpcContext {
                client_instance_id: ClientInstanceId::SYSTEM,
                selected_protocol_minor: 27,
                cancellation: Cancellation::new(),
                deadline: Some(Instant::now() + Duration::from_secs(1)),
                effective_budget: Some(FirstSliceEffectiveBudget {
                    rows: 2,
                    edges: 1,
                    results: 1,
                    source_bytes: 1,
                    json_bytes: 4096,
                    estimated_tokens: 4096,
                    memory_bytes: 4096,
                    duration: Duration::from_secs(1),
                    depth: None,
                    paths: None,
                    source_verification_duration_explicit: true,
                }),
                index_admission: None,
            },
        )
        .await;
        assert_eq!(handler.calls.load(Ordering::SeqCst), 1);
        assert!(
            matches!(response, daemon::response_envelope::Response::Error(_)),
            "invalid handler output cannot cross transport"
        );
        let (request, mut response) = fixture();
        response.matches[0].end_byte = 99;
        assert!(!first_slice_response_correlates_for_minor(
            &FirstSliceIpcRequest::SourceFind(request),
            &FirstSliceIpcResponse::SourceFind(response),
            27
        ));
    }
}
