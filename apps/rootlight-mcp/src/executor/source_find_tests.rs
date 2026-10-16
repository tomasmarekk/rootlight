//! Source-only executor evidence uses immutable fake-port pages to exercise
//! authentication before access and exact physical spans after transport.
//! No source bodies are returned or reconstructed by this boundary.

use super::*;
use rootlight_mcp_contract::source_find::{SourceFindEnvelope, SourceFindOutput, SourceFindStop};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ObservedSourceFind {
    pub(super) repository: RepositoryId,
    pub(super) generation: client::GenerationSelector,
    pub(super) file: FileId,
    pub(super) literal: String,
    pub(super) search: client::SourceFindOptions,
}

fn arguments(literal: &str) -> Value {
    json!({"repository": {"repository_id": repository()}, "file_id": file(), "literal": literal})
}

fn outcome(pages: Vec<client::SourceFindResult>) -> FakeOutcome {
    FakeOutcome::SourceFindSequence(Arc::new(Mutex::new(pages.into_iter().map(Ok).collect())))
}

fn physical(text: &str, encoding: client::SourceTextEncoding) -> Vec<u8> {
    match encoding {
        client::SourceTextEncoding::Utf8 => text.as_bytes().to_vec(),
        client::SourceTextEncoding::Utf16Le | client::SourceTextEncoding::Utf16Be => {
            let little = encoding == client::SourceTextEncoding::Utf16Le;
            let mut bytes = if little {
                vec![0xff, 0xfe]
            } else {
                vec![0xfe, 0xff]
            };
            for unit in text.encode_utf16() {
                bytes.extend(if little {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
            bytes
        }
    }
}

fn page(
    bytes: &[u8],
    encoding: client::SourceTextEncoding,
    scanned: std::ops::Range<u64>,
    matches: &[std::ops::Range<u64>],
    prefix: Option<u64>,
    restored: u64,
) -> client::SourceFindResult {
    let hash = rootlight_ids::content_hash(bytes);
    let metadata = client::SourceFindFile {
        file: file(),
        path: "src/fixture.e".to_owned(),
        language: "eiffel".to_owned(),
        content_hash: hash,
        byte_length: u64::try_from(bytes.len()).unwrap(),
        encoding,
        generated: false,
        extracted_from: None,
    };
    client::SourceFindResult {
        repository: repository(),
        generation: generation(),
        total_metadata_bytes: u64::try_from(metadata.path.len() + metadata.language.len()).unwrap(),
        total_response_memory_bytes: 1024,
        file: metadata,
        matches: matches
            .iter()
            .map(|range| {
                client::SourceReference::new(
                    repository(),
                    generation(),
                    file(),
                    range.clone(),
                    hash,
                    None,
                )
                .unwrap()
            })
            .collect(),
        continuation: prefix.map(|matched_prefix| client::SourceFindCursor {
            next_byte: scanned.end,
            matched_prefix,
        }),
        stop: if prefix.is_some() {
            client::SourceFindStop::MatchLimit
        } else {
            client::SourceFindStop::EndOfSource
        },
        scanned,
        restoration_bytes: restored,
    }
}

async fn execute(
    harness: &Harness,
    arguments: Value,
    exposure: ExposureProfile,
    cancellation: RequestCancellation,
) -> Result<Map<String, Value>, ToolExecutionError> {
    execute_source_find(
        Arc::clone(&harness.executor.port),
        arguments.as_object().unwrap().clone(),
        exposure,
        cancellation,
        &harness.executor.unsupported,
        &harness.executor.invalid_arguments,
        &harness.executor.invalid_cursor,
        harness.executor.cursor_key,
    )
    .await
}

fn success(wire: Map<String, Value>) -> SourceFindEnvelope {
    let serialized = serde_json::to_vec(&wire).unwrap();
    let decoded: SourceFindOutput = serde_json::from_value(Value::Object(wire.clone())).unwrap();
    let SourceFindOutput::Success(output) = decoded else {
        panic!("expected exact source success")
    };
    assert_eq!(
        output.usage.json_bytes,
        u64::try_from(serialized.len()).unwrap()
    );
    assert_eq!(
        output.usage.estimated_tokens,
        rootlight_mcp_contract::accounting::estimate_tokens(serialized.len())
    );
    assert_eq!(output.usage.source_bytes, 0);
    for field in [
        "content",
        "literal",
        "total_matches",
        "matched_candidates",
        "coverage",
    ] {
        assert!(wire["data"].get(field).is_none(), "unexpected {field}");
    }
    output
}

fn resume_arguments(first: &SourceFindEnvelope) -> Value {
    let mut request = arguments("aaa");
    request["generation"] = json!(generation());
    request["maximum_matches"] = json!(1);
    request["cursor"] = serde_json::to_value(first.next_cursor.0.as_ref().unwrap()).unwrap();
    request
}

async fn first_page() -> SourceFindEnvelope {
    let harness = Harness::with_cursor_key(
        outcome(vec![page(
            b"aaaaa",
            client::SourceTextEncoding::Utf8,
            0..3,
            std::slice::from_ref(&(0..3)),
            Some(2),
            0,
        )]),
        [11; 32],
    );
    let mut request = arguments("aaa");
    request["maximum_matches"] = json!(1);
    success(
        execute(
            &harness,
            request,
            ExposureProfile::Developer,
            cancellation(),
        )
        .await
        .unwrap(),
    )
}

#[tokio::test]
async fn authenticated_pages_preserve_overlaps_and_physical_spans_in_three_encodings() {
    for encoding in [
        client::SourceTextEncoding::Utf8,
        client::SourceTextEncoding::Utf16Le,
        client::SourceTextEncoding::Utf16Be,
    ] {
        let bytes = physical("aaaaa", encoding);
        let (origin, width) = if encoding == client::SourceTextEncoding::Utf8 {
            (0, 1)
        } else {
            (2, 2)
        };
        let ranges = [
            origin..origin + 3 * width,
            origin + width..origin + 4 * width,
            origin + 2 * width..origin + 5 * width,
        ];
        let pages = vec![
            page(
                &bytes,
                encoding,
                origin..ranges[0].end,
                &ranges[0..1],
                Some(2),
                0,
            ),
            page(
                &bytes,
                encoding,
                ranges[0].end..ranges[1].end,
                &ranges[1..2],
                Some(2),
                3 * width,
            ),
            page(
                &bytes,
                encoding,
                ranges[1].end..ranges[2].end,
                &ranges[2..3],
                None,
                3 * width,
            ),
        ];
        let harness = Harness::with_cursor_key(outcome(pages), [11; 32]);
        let mut request = arguments("aaa");
        request["maximum_matches"] = json!(1);
        for (index, expected) in ranges.iter().enumerate() {
            let output = success(
                execute(
                    &harness,
                    request.clone(),
                    ExposureProfile::Developer,
                    cancellation(),
                )
                .await
                .unwrap(),
            );
            assert_eq!(output.generation, generation());
            assert_eq!(output.data.matches.len(), 1);
            let source = &output.data.matches[0];
            assert_eq!(
                source.span().start_byte()..source.span().end_byte(),
                *expected
            );
            assert_eq!(source.content_hash(), rootlight_ids::content_hash(&bytes));
            assert_eq!(source.span().file(), file());
            assert_eq!(source.repository(), repository());
            assert_eq!(source.generation(), generation());
            assert_eq!(output.truncated, index != 2);
            assert_eq!(
                output.data.stop,
                if index == 2 {
                    SourceFindStop::EndOfSource
                } else {
                    SourceFindStop::MatchLimit
                }
            );
            if index != 2 {
                request = resume_arguments(&output);
            }
        }
        let calls = harness.calls.lock().unwrap();
        let finds: Vec<_> = calls
            .iter()
            .filter_map(|call| {
                if let ObservedCall::SourceFind(find) = call {
                    Some(find)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(finds.len(), 3);
        assert!(finds.iter().all(|find| find.generation
            == client::GenerationSelector::Generation(generation())
            && find.literal == "aaa"
            && find.file == file()));
        assert_eq!(finds[1].search.cursor.unwrap().next_byte, ranges[0].end);
        assert_eq!(finds[1].search.cursor.unwrap().matched_prefix, 2);
    }
}

#[tokio::test]
async fn complete_unicode_and_zero_hit_pages_have_exact_usage_and_no_invented_total() {
    for encoding in [
        client::SourceTextEncoding::Utf8,
        client::SourceTextEncoding::Utf16Le,
        client::SourceTextEncoding::Utf16Be,
    ] {
        let bytes = physical("x😀λx", encoding);
        let (range, origin) = if encoding == client::SourceTextEncoding::Utf8 {
            (1..7, 0)
        } else {
            (4..10, 2)
        };
        let end = u64::try_from(bytes.len()).unwrap();
        let harness = Harness::new(outcome(vec![page(
            &bytes,
            encoding,
            origin..end,
            std::slice::from_ref(&range),
            None,
            0,
        )]));
        let output = success(
            execute(
                &harness,
                arguments("😀λ"),
                ExposureProfile::Developer,
                cancellation(),
            )
            .await
            .unwrap(),
        );
        assert_eq!(
            output.data.matches[0].span().start_byte()..output.data.matches[0].span().end_byte(),
            range
        );
        assert!(output.next_cursor.0.is_none());
        let harness = Harness::new(outcome(vec![page(
            &bytes,
            encoding,
            origin..end,
            &[],
            None,
            0,
        )]));
        let output = success(
            execute(
                &harness,
                arguments("absent"),
                ExposureProfile::Developer,
                cancellation(),
            )
            .await
            .unwrap(),
        );
        assert!(output.data.matches.is_empty());
        assert_eq!(output.data.stop, SourceFindStop::EndOfSource);
    }
    let harness = Harness::new(outcome(vec![page(
        b"",
        client::SourceTextEncoding::Utf8,
        0..0,
        &[],
        None,
        0,
    )]));
    let output = success(
        execute(
            &harness,
            arguments("x"),
            ExposureProfile::Developer,
            cancellation(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(output.data.scanned_end_byte, 0);
    assert!(!output.truncated);
}

#[tokio::test]
async fn modified_authenticated_request_scope_is_rejected_before_any_port_access() {
    let first = first_page().await;
    let original = resume_arguments(&first);
    for mutation in 0..9 {
        let mut request = original.clone();
        let mut key = [11; 32];
        let mut exposure = ExposureProfile::Developer;
        match mutation {
            0 => request["literal"] = json!("aab"),
            1 => request["file_id"] = json!(FileId::from_bytes([19; 20])),
            2 => request["generation"] = json!(GenerationId::from_bytes([19; 20])),
            3 => request["maximum_matches"] = json!(2),
            4 => request["maximum_scan_bytes"] = json!(16),
            5 => request["budget"] = json!({"max_tokens": 1000}),
            6 => exposure = ExposureProfile::Scout,
            7 => key = [12; 32],
            _ => request["repository"]["repository_id"] = json!(RepositoryId::from_bytes([19; 16])),
        }
        let harness = Harness::with_cursor_key(outcome(vec![]), key);
        let error = execute(&harness, request, exposure, cancellation())
            .await
            .unwrap_err();
        assert_eq!(
            error.public_error().map(PublicError::code),
            Some(ErrorCode::InvalidCursor),
            "mutation {mutation}"
        );
        assert_eq!(harness.prepare_count.load(Ordering::Relaxed), 0);
        assert_eq!(harness.call_count.load(Ordering::Relaxed), 0);
    }
}

#[tokio::test]
async fn unsupported_response_profiles_fail_before_any_port_access() {
    let first = first_page().await;
    for original in [arguments("aaa"), resume_arguments(&first)] {
        for profile in ["standard", "evidence"] {
            let harness = Harness::new(outcome(vec![]));
            let mut request = original.clone();
            request["response_profile"] = json!(profile);
            let error = execute(
                &harness,
                request,
                ExposureProfile::Developer,
                cancellation(),
            )
            .await
            .unwrap_err();
            assert_eq!(
                error.public_error().map(PublicError::code),
                Some(ErrorCode::InvalidArgument)
            );
            assert_eq!(harness.prepare_count.load(Ordering::Relaxed), 0);
            assert_eq!(harness.call_count.load(Ordering::Relaxed), 0);
        }
    }
}

#[tokio::test]
async fn authenticated_file_metadata_cannot_change_between_pages() {
    let first = first_page().await;
    for mutation in 0..6 {
        let mut result = page(
            b"aaaaa",
            client::SourceTextEncoding::Utf8,
            3..4,
            std::slice::from_ref(&(1..4)),
            Some(2),
            3,
        );
        match mutation {
            0 => result.file.path = "src/other.e".to_owned(),
            1 => result.file.language = "rust".to_owned(),
            2 => result.file.content_hash = ContentHash::from_bytes([19; 32]),
            3 => result.file.generated = true,
            4 => result.file.byte_length = 6,
            _ => {
                result.file.extracted_from = Some(
                    client::SourceReference::new(
                        repository(),
                        generation(),
                        FileId::from_bytes([19; 20]),
                        0..20,
                        content_hash(),
                        None,
                    )
                    .unwrap(),
                )
            }
        }
        result.total_metadata_bytes =
            u64::try_from(result.file.path.len() + result.file.language.len()).unwrap();
        let harness = Harness::with_cursor_key(outcome(vec![result]), [11; 32]);
        let error = execute(
            &harness,
            resume_arguments(&first),
            ExposureProfile::Developer,
            cancellation(),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.failure(),
            Some(ToolExecutionFailure::InvalidResponse),
            "mutation {mutation}"
        );
        assert_eq!(harness.call_count.load(Ordering::Relaxed), 2);
    }
}

#[tokio::test]
async fn untrusted_port_cannot_forge_match_identity_memory_or_scan_progress() {
    for mutation in 0..13 {
        let mut result = page(
            b"aaaaaa",
            client::SourceTextEncoding::Utf8,
            0..3,
            std::slice::from_ref(&(0..3)),
            Some(2),
            0,
        );
        match mutation {
            0 => result.repository = RepositoryId::from_bytes([19; 16]),
            1 => result.generation = GenerationId::from_bytes([19; 20]),
            2 => result.file.file = FileId::from_bytes([19; 20]),
            3 => {
                result.matches[0] = client::SourceReference::new(
                    repository(),
                    generation(),
                    file(),
                    0..3,
                    content_hash(),
                    None,
                )
                .unwrap()
            }
            4 => {
                result.matches[0] = client::SourceReference::new(
                    repository(),
                    generation(),
                    file(),
                    0..2,
                    result.file.content_hash,
                    None,
                )
                .unwrap()
            }
            5 => result.scanned.start = 1,
            6 => result.restoration_bytes = 1,
            7 => result.scanned.end = 4,
            8 => {
                result.stop = client::SourceFindStop::ScanLimit;
                result.matches.clear();
            }
            9 => result.continuation.as_mut().unwrap().next_byte = 2,
            10 => result.continuation.as_mut().unwrap().matched_prefix = 3,
            11 => result.total_metadata_bytes += 1,
            _ => result.total_response_memory_bytes = u64::MAX,
        }
        let harness = Harness::new(outcome(vec![result]));
        let mut request = arguments("aaa");
        request["maximum_matches"] = json!(1);
        let error = execute(
            &harness,
            request,
            ExposureProfile::Developer,
            cancellation(),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.failure(),
            Some(ToolExecutionFailure::InvalidResponse),
            "mutation {mutation}"
        );
    }
}

#[tokio::test]
async fn resumed_restoration_and_prefix_cannot_claim_unexamined_source_bytes() {
    let first = first_page().await;
    for restored in [0, 1, 4, u64::MAX] {
        let result = page(
            b"aaaaa",
            client::SourceTextEncoding::Utf8,
            3..4,
            std::slice::from_ref(&(1..4)),
            Some(2),
            restored,
        );
        let harness = Harness::with_cursor_key(outcome(vec![result]), [11; 32]);
        let error = execute(
            &harness,
            resume_arguments(&first),
            ExposureProfile::Developer,
            cancellation(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.failure(), Some(ToolExecutionFailure::InvalidResponse));
    }
    let mut result = page(
        b"aaaaa",
        client::SourceTextEncoding::Utf8,
        0..1,
        &[],
        Some(2),
        0,
    );
    result.stop = client::SourceFindStop::ScanLimit;
    let harness = Harness::new(outcome(vec![result]));
    let mut request = arguments("aaa");
    request["maximum_scan_bytes"] = json!(1);
    let error = execute(
        &harness,
        request,
        ExposureProfile::Developer,
        cancellation(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.failure(), Some(ToolExecutionFailure::InvalidResponse));
}

#[tokio::test]
async fn transport_result_limit_is_explicit_and_output_budget_failure_never_trims() {
    let mut result = page(
        b"aaaaa",
        client::SourceTextEncoding::Utf8,
        0..3,
        std::slice::from_ref(&(0..3)),
        Some(2),
        0,
    );
    result.stop = client::SourceFindStop::ResultLimit;
    let harness = Harness::new(outcome(vec![result]));
    let mut request = arguments("aaa");
    request["maximum_matches"] = json!(3);
    request["budget"] = json!({"max_results":1});
    let output = success(
        execute(
            &harness,
            request,
            ExposureProfile::Developer,
            cancellation(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(output.data.stop, SourceFindStop::ResultLimit);
    assert_eq!(output.data.matches.len(), 1);
    assert!(output.next_cursor.0.is_some());
    let harness = Harness::new(outcome(vec![page(
        b"aaa",
        client::SourceTextEncoding::Utf8,
        0..3,
        std::slice::from_ref(&(0..3)),
        None,
        0,
    )]));
    let mut request = arguments("aaa");
    request["budget"] = json!({"max_tokens":100});
    let error = execute(
        &harness,
        request,
        ExposureProfile::Developer,
        cancellation(),
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.public_error().map(PublicError::code),
        Some(ErrorCode::BudgetExceeded)
    );
}

#[tokio::test]
async fn cancellation_drops_an_inflight_source_scan() {
    let started = Arc::new(Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let harness = Harness::new(FakeOutcome::PendingSourceFind {
        started: Arc::clone(&started),
        dropped: Arc::clone(&dropped),
    });
    let (sender, receiver) = watch::channel(false);
    let operation = execute(
        &harness,
        arguments("aaa"),
        ExposureProfile::Developer,
        RequestCancellation { receiver },
    );
    let cancel = async {
        started.notified().await;
        sender.send(true).unwrap();
    };
    let (result, ()) = tokio::join!(operation, cancel);
    assert_eq!(
        result.unwrap_err().failure(),
        Some(ToolExecutionFailure::Executor)
    );
    assert!(dropped.load(Ordering::Relaxed));
}

#[tokio::test]
async fn explicit_forward_and_transport_controls_reach_the_source_port() {
    let mut result = page(
        b"xy",
        client::SourceTextEncoding::Utf8,
        0..1,
        &[],
        Some(0),
        0,
    );
    result.stop = client::SourceFindStop::ScanLimit;
    let harness = Harness::new(outcome(vec![result]));
    let mut request = arguments("absent");
    request["generation"] = json!(generation());
    request["maximum_scan_bytes"] = json!(1);
    request["maximum_matches"] = json!(5);
    request["budget"] = json!({"max_results": 7, "max_tokens": 3000, "timeout_ms": 500});
    request["response_profile"] = json!("compact");
    let output = success(
        execute(
            &harness,
            request,
            ExposureProfile::Developer,
            cancellation(),
        )
        .await
        .unwrap(),
    );
    assert!(output.data.matches.is_empty());
    assert_eq!(output.data.stop, SourceFindStop::ScanLimit);
    assert!(output.truncated && output.next_cursor.0.is_some());
    let calls = harness.calls.lock().unwrap();
    let ObservedCall::SourceFind(find) = calls.last().unwrap() else {
        panic!("expected source port")
    };
    assert_eq!(find.search.maximum_scan_bytes, 1);
    assert_eq!(find.search.maximum_matches, 5);
    assert_eq!(
        find.generation,
        client::GenerationSelector::Generation(generation())
    );
    let expected = AnalyticalBudget::new(Some(
        &serde_json::from_value(json!({"max_results": 7, "max_tokens": 3000, "timeout_ms": 500}))
            .unwrap(),
    ))
    .unwrap()
    .options;
    assert_eq!(
        find.options,
        expected.with_effective_budget(
            expected
                .effective_budget()
                .unwrap()
                .with_source_verification_duration_explicit(true)
        )
    );
}

#[tokio::test]
async fn invalid_queries_and_unauthenticated_cursors_fail_without_source_access() {
    for mutation in 0..4 {
        let mut request = arguments("aaa");
        match mutation {
            0 => request["literal"] = json!(""),
            1 => request["literal"] = json!("a".repeat(65_537)),
            2 => {
                request["cursor"] = json!("forged");
                request["generation"] = json!(generation());
            }
            _ => {
                request["cursor"] = json!("forged");
                request["generation"] = json!("active");
            }
        }
        let harness = Harness::new(outcome(vec![]));
        assert!(
            execute(
                &harness,
                request,
                ExposureProfile::Developer,
                cancellation()
            )
            .await
            .is_err()
        );
        assert_eq!(harness.prepare_count.load(Ordering::Relaxed), 0);
        assert_eq!(harness.call_count.load(Ordering::Relaxed), 0);
    }
}
