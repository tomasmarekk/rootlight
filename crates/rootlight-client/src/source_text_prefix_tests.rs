//! Negotiated source-prefix admission with unchanged approved physical references.
//! Provider text is bounded independently of physical bytes and execution completeness.

use super::*;
use crate::tests::{test_repository, test_source, wire_query_context, wire_source_read};

fn projection() -> SourceTextReadOptions {
    SourceTextReadOptions {
        include_line_numbers: false,
        ..SourceTextReadOptions::new()
    }
}

fn wire_prefix(
    source: &SourceReference,
    text: &str,
    encoding: daemon::SourcePhysicalEncoding,
    range: std::ops::Range<u64>,
    clipped: bool,
) -> daemon::SourceReadResponse {
    let mut response = wire_source_read(source);
    let width = range.end - range.start;
    response.context = Some(wire_query_context(1, width));
    response.total_source_bytes = width;
    response.total_text_bytes = u64::try_from(text.len()).unwrap();
    response.prefix_truncated = Some(clipped);
    response.completeness = Some(daemon::FirstSliceCompleteness {
        state: daemon::FirstSliceCompletenessState::FirstSliceCompletenessComplete as i32,
        continuation:
            daemon::FirstSliceContinuationAvailability::FirstSliceContinuationNotApplicable as i32,
        ..Default::default()
    });
    let chunk = &mut response.chunks[0];
    chunk.content = text.to_owned();
    chunk.encoding = daemon::SourceReadEncoding::LosslessText as i32;
    chunk.physical_encoding = Some(encoding as i32);
    chunk.start_byte = range.start;
    chunk.end_byte = range.end;
    chunk.start_line = 0;
    chunk.end_line = 0;
    chunk.included_start_line = None;
    chunk.included_end_line = None;
    chunk.exact_content.clear();
    response
}

fn parse(
    response: daemon::SourceReadResponse,
    sources: &[SourceReference],
    projection: SourceTextReadOptions,
    cap: u32,
) -> Result<SourceTextPrefixReadResult, ClientError> {
    parse_source_text_prefix_read_for_minor(
        response,
        test_repository(),
        GenerationSelector::Active,
        sources,
        projection,
        cap,
        CURRENT_PROTOCOL_MINOR,
    )
}

#[test]
fn lossless_prefix_requires_new_minor_and_rejects_invalid_controls() {
    let source = test_source(4, 0, 3);
    let sources = std::slice::from_ref(&source);
    let request = build_source_text_prefix_read_request(
        test_repository(),
        GenerationSelector::Active,
        sources,
        projection(),
        3,
    )
    .unwrap();
    for minor in MINIMUM_PROTOCOL_MINOR..25 {
        assert!(matches!(
            ensure_request_supported(&request, minor),
            Err(ClientError::ProtocolFeatureUnavailable)
        ));
    }
    assert!(ensure_request_supported(&request, 25).is_ok());
    for cap in [0, 512 * 1024 + 1] {
        assert!(
            build_source_text_prefix_read_request(
                test_repository(),
                GenerationSelector::Active,
                sources,
                projection(),
                cap
            )
            .is_err()
        );
    }
    for options in [
        SourceTextReadOptions::new(),
        SourceTextReadOptions {
            merge_overlaps: true,
            ..projection()
        },
    ] {
        assert!(
            build_source_text_prefix_read_request(
                test_repository(),
                GenerationSelector::Active,
                sources,
                options,
                3
            )
            .is_err()
        );
    }
    let daemon::request_envelope::Request::SourceRead(mut wire) = request else {
        panic!("source read request expected");
    };
    assert_eq!(wire.maximum_text_bytes_per_chunk, Some(3));
    assert_eq!(wire.include_line_numbers, Some(false));
    for encoding in [
        daemon::SourceReadEncoding::Utf8,
        daemon::SourceReadEncoding::Bytes,
    ] {
        wire.encoding = encoding as i32;
        assert!(matches!(
            ensure_request_supported(
                &daemon::request_envelope::Request::SourceRead(wire.clone()),
                25
            ),
            Err(ClientError::InvalidFirstSliceRequest)
        ));
    }
    let exact = build_source_text_read_request(
        test_repository(),
        GenerationSelector::Active,
        sources,
        projection(),
    )
    .unwrap();
    assert!(ensure_request_supported(&exact, 24).is_ok());
    let daemon::request_envelope::Request::SourceRead(exact) = exact else {
        unreachable!()
    };
    assert_eq!(exact.maximum_text_bytes_per_chunk, None);
}

#[test]
fn lossless_prefix_preserves_bom_crlf_and_distinct_physical_text_totals() {
    let text = "\u{feff}a\r\n☃🧭\r\n";
    for encoding in [
        daemon::SourcePhysicalEncoding::Utf8,
        daemon::SourcePhysicalEncoding::Utf16Le,
        daemon::SourcePhysicalEncoding::Utf16Be,
    ] {
        let width = if encoding == daemon::SourcePhysicalEncoding::Utf8 {
            text.len()
        } else {
            text.encode_utf16().count() * 2
        };
        let width = u64::try_from(width).unwrap();
        let source = test_source(4, 0, width);
        let response = wire_prefix(&source, text, encoding, 0..width, false);
        let parsed = parse(
            response,
            std::slice::from_ref(&source),
            projection(),
            u32::try_from(text.len()).unwrap(),
        )
        .unwrap();
        assert_eq!(parsed.result.chunks[0].source, source);
        assert_eq!(parsed.result.chunks[0].content_hash, source.content_hash);
        assert_eq!(parsed.result.chunks[0].content, text);
        assert_eq!(parsed.result.total_source_bytes, width);
        assert_eq!(
            parsed.result.total_text_bytes,
            u64::try_from(text.len()).unwrap()
        );
        assert!(!parsed.prefix_truncated);
        assert!(!parsed.result.truncated);
        assert!(!format!("{parsed:?}").contains("☃"));
    }
}

#[test]
fn lossless_prefix_clipping_is_not_execution_truncation_and_keeps_full_reference() {
    for encoding in [
        daemon::SourcePhysicalEncoding::Utf16Le,
        daemon::SourcePhysicalEncoding::Utf16Be,
    ] {
        let source = test_source(4, 0, 20);
        for (text, range, cap) in [
            ("", 0..0, 1),
            ("\u{feff}", 0..2, 3),
            ("\u{feff}a🧭", 0..8, 8),
        ] {
            let parsed = parse(
                wire_prefix(&source, text, encoding, range.clone(), true),
                std::slice::from_ref(&source),
                projection(),
                cap,
            )
            .unwrap();
            assert_eq!(parsed.result.chunks[0].source, source);
            assert_eq!(parsed.result.chunks[0].end_byte, range.end);
            assert_eq!(parsed.result.chunks[0].content, text);
            assert!(parsed.prefix_truncated);
            assert!(!parsed.result.truncated);
            assert_eq!(
                parsed.result.execution_completeness.state,
                ResultCompletenessState::Complete
            );
        }
        let empty = test_source(4, 0, 0);
        assert!(
            !parse(
                wire_prefix(&empty, "", encoding, 0..0, false),
                std::slice::from_ref(&empty),
                projection(),
                1
            )
            .unwrap()
            .prefix_truncated
        );
    }
}

#[test]
fn lossless_prefix_context_can_end_before_selection_and_clip_only_suffix_context() {
    for encoding in [
        daemon::SourcePhysicalEncoding::Utf16Le,
        daemon::SourcePhysicalEncoding::Utf16Be,
    ] {
        let source = test_source(4, 10, 14);
        let options = SourceTextReadOptions {
            context_lines_before: 1,
            context_lines_after: 1,
            ..projection()
        };
        let parsed = parse(
            wire_prefix(&source, "\u{feff}a", encoding, 0..4, true),
            std::slice::from_ref(&source),
            options,
            4,
        )
        .unwrap();
        assert_eq!(parsed.result.chunks[0].source, source);
        assert!(parsed.result.chunks[0].end_byte < source.start_byte);
        assert!(parsed.prefix_truncated);
        let selected = test_source(4, 2, 4);
        for clipped in [false, true] {
            let parsed = parse(
                wire_prefix(&selected, "ab", encoding, 2..6, clipped),
                std::slice::from_ref(&selected),
                options,
                2,
            )
            .unwrap();
            assert_eq!(parsed.prefix_truncated, clipped);
            assert_eq!(parsed.result.chunks[0].source, selected);
        }
        let bad = test_source(4, 6, 8);
        assert!(
            parse(
                wire_prefix(&bad, "\u{feff}a🧭", encoding, 0..8, false),
                std::slice::from_ref(&bad),
                options,
                8
            )
            .is_err()
        );
    }
}

#[test]
fn lossless_prefix_rejects_inconsistent_flags_totals_controls_and_identity() {
    for encoding in [
        daemon::SourcePhysicalEncoding::Utf16Le,
        daemon::SourcePhysicalEncoding::Utf16Be,
    ] {
        let source = test_source(4, 0, 12);
        let good = wire_prefix(&source, "\u{feff}ab", encoding, 0..6, true);
        assert!(parse(good.clone(), std::slice::from_ref(&source), projection(), 5).is_ok());
        let mut bad = Vec::new();
        let mut wire = good.clone();
        wire.prefix_truncated = None;
        bad.push(wire);
        let mut wire = good.clone();
        wire.prefix_truncated = Some(false);
        bad.push(wire);
        let mut wire = good.clone();
        wire.total_source_bytes += 1;
        bad.push(wire);
        let mut wire = good.clone();
        wire.total_text_bytes += 1;
        bad.push(wire);
        let mut wire = good.clone();
        wire.chunks[0].content.push('x');
        bad.push(wire);
        let mut wire = good.clone();
        wire.chunks[0].physical_encoding = None;
        bad.push(wire);
        let mut wire = good.clone();
        wire.chunks[0].physical_encoding = Some(99);
        bad.push(wire);
        let mut wire = good.clone();
        wire.chunks[0].exact_content = vec![0];
        bad.push(wire);
        let mut wire = good.clone();
        wire.chunks[0].included_start_line = Some(1);
        bad.push(wire);
        let mut wire = good.clone();
        wire.chunks[0].start_line = 1;
        bad.push(wire);
        let mut wire = good.clone();
        wire.chunks[0].start_byte = 2;
        bad.push(wire);
        let mut wire = good.clone();
        wire.chunks[0].end_byte = 5;
        bad.push(wire);
        let mut wire = good.clone();
        wire.chunks[0].source = Some(source_reference_to_wire(&test_source(4, 0, 6)));
        bad.push(wire);
        let mut wire = good.clone();
        wire.chunks[0].content_hash = Some(content_hash_to_wire(ContentHash::from_bytes([9; 32])));
        bad.push(wire);
        let mut wire = good.clone();
        wire.chunks[0].source.as_mut().unwrap().generation =
            Some(generation_to_wire(GenerationId::from_bytes([9; 20])));
        bad.push(wire);
        let mut wire = good.clone();
        wire.context
            .as_mut()
            .unwrap()
            .usage
            .as_mut()
            .unwrap()
            .source_bytes += 1;
        bad.push(wire);
        for (index, wire) in bad.into_iter().enumerate() {
            assert!(
                parse(wire, std::slice::from_ref(&source), projection(), 5).is_err(),
                "invalid provider case {index}"
            );
        }
        assert!(parse(good, std::slice::from_ref(&source), projection(), 4).is_err());
    }
}

#[test]
fn lossless_prefix_checks_visible_scalar_endpoints_bom_and_encoding_policy() {
    for encoding in [
        daemon::SourcePhysicalEncoding::Utf16Le,
        daemon::SourcePhysicalEncoding::Utf16Be,
    ] {
        let source = test_source(4, 0, 12);
        for (text, range) in [("ab", 0..4), ("\u{feff}\0", 0..4)] {
            assert!(
                parse(
                    wire_prefix(&source, text, encoding, range, true),
                    std::slice::from_ref(&source),
                    projection(),
                    8
                )
                .is_err()
            );
        }
        let odd = test_source(4, 2, 7);
        assert!(
            parse(
                wire_prefix(&odd, "a", encoding, 2..4, true),
                std::slice::from_ref(&odd),
                projection(),
                1
            )
            .is_err()
        );
        let pair_end = test_source(4, 2, 6);
        let options = SourceTextReadOptions {
            context_lines_after: 1,
            ..projection()
        };
        assert!(
            parse(
                wire_prefix(&pair_end, "a🧭", encoding, 2..8, false),
                std::slice::from_ref(&pair_end),
                options,
                5
            )
            .is_err()
        );
        let full = test_source(4, 2, 4);
        assert!(
            parse(
                wire_prefix(&full, "a", encoding, 2..4, true),
                std::slice::from_ref(&full),
                projection(),
                1
            )
            .is_err()
        );
    }
    let utf8 = test_source(4, 0, 9);
    assert!(
        parse(
            wire_prefix(
                &utf8,
                "a\0",
                daemon::SourcePhysicalEncoding::Utf8,
                0..2,
                true
            ),
            std::slice::from_ref(&utf8),
            projection(),
            2
        )
        .is_ok()
    );
}

#[test]
fn lossless_prefix_is_never_admitted_as_an_exact_or_legacy_read() {
    let source = test_source(4, 0, 3);
    let sources = std::slice::from_ref(&source);
    for clipped in [false, true] {
        let wire = wire_prefix(
            &source,
            "abc",
            daemon::SourcePhysicalEncoding::Utf8,
            0..3,
            clipped,
        );
        assert!(
            parse_source_text_read_for_minor(
                wire,
                test_repository(),
                GenerationSelector::Active,
                sources,
                projection(),
                25
            )
            .is_err()
        );
        let mut legacy = wire_source_read(&source);
        legacy.prefix_truncated = Some(clipped);
        assert!(
            parse_source_read(
                legacy,
                test_repository(),
                GenerationSelector::Active,
                sources,
                SourceReadOptions::new()
            )
            .is_err()
        );
    }
    assert!(matches!(
        parse_source_text_prefix_read_for_minor(
            wire_prefix(
                &source,
                "abc",
                daemon::SourcePhysicalEncoding::Utf8,
                0..3,
                false
            ),
            test_repository(),
            GenerationSelector::Active,
            sources,
            projection(),
            3,
            24
        ),
        Err(ClientError::ProtocolFeatureUnavailable)
    ));
}

#[test]
fn lossless_prefix_preserves_order_and_separates_execution_row_truncation() {
    let first = test_source(4, 2, 8);
    let second = test_source(5, 2, 8);
    let mut wire = wire_prefix(
        &first,
        "a",
        daemon::SourcePhysicalEncoding::Utf16Le,
        2..4,
        true,
    );
    wire.chunks.extend(
        wire_prefix(
            &second,
            "b",
            daemon::SourcePhysicalEncoding::Utf16Be,
            2..4,
            true,
        )
        .chunks,
    );
    wire.context = Some(wire_query_context(2, 4));
    wire.total_source_bytes = 4;
    wire.total_text_bytes = 2;
    let sources = [first, second];
    assert!(parse(wire.clone(), &sources, projection(), 1).is_ok());
    let mut reordered = wire.clone();
    reordered.chunks.swap(0, 1);
    assert!(parse(reordered, &sources, projection(), 1).is_err());
    wire.chunks.pop();
    wire.context = Some(wire_query_context(1, 2));
    wire.total_source_bytes = 2;
    wire.total_text_bytes = 1;
    assert!(parse(wire.clone(), &sources, projection(), 1).is_err());
    wire.truncated = true;
    wire.completeness = None;
    let parsed = parse(wire, &sources, projection(), 1).unwrap();
    assert_eq!(parsed.result.chunks[0].source, sources[0]);
    assert!(parsed.prefix_truncated);
    assert!(parsed.result.truncated);
}

#[test]
fn lossless_prefix_preserves_container_origin_and_requires_actual_range_activity() {
    let selected = test_source(4, 0, 12);
    let actual = test_source(4, 0, 4);
    let origin = SourceReference::new(
        test_repository(),
        crate::tests::test_generation(),
        FileId::from_bytes([7; 20]),
        0..1024,
        ContentHash::from_bytes([7; 32]),
        None,
    )
    .unwrap();
    let mut wire = wire_prefix(
        &selected,
        "\u{feff}a",
        daemon::SourcePhysicalEncoding::Utf16Be,
        0..4,
        true,
    );
    wire.chunks[0].language = "vba".to_owned();
    wire.chunks[0].extracted_from = Some(source_reference_to_wire(&origin));
    wire.conditional_activity = vec![crate::tests::wire_activity(
        &actual,
        daemon::ConditionalActivityBasis::ReturnedRange,
    )];
    let parsed = parse(
        wire.clone(),
        std::slice::from_ref(&selected),
        projection(),
        4,
    )
    .unwrap();
    assert_eq!(parsed.result.chunks[0].source, selected);
    assert_eq!(parsed.result.chunks[0].extracted_from, Some(origin));
    wire.conditional_activity = vec![crate::tests::wire_activity(
        &selected,
        daemon::ConditionalActivityBasis::ReturnedRange,
    )];
    assert!(parse(wire, std::slice::from_ref(&selected), projection(), 4).is_err());
}

#[tokio::test]
async fn lossless_prefix_older_minor_closes_before_writing_an_operation() {
    use rootlight_ipc::{AsyncLocalListener, read_client_hello_async, write_server_hello_async};
    use tokio::io::AsyncReadExt as _;
    let (_temporary, endpoint) = crate::tests::async_test_endpoint("prefix-old-minor");
    let listener = AsyncLocalListener::bind(endpoint.clone()).unwrap();
    let nonce = [16; 16];
    let client = Client::new(endpoint, nonce, [17; 16]);
    let server = tokio::spawn(async move {
        let mut stream = listener
            .accept_timeout(Duration::from_secs(2))
            .await
            .unwrap();
        let hello = read_client_hello_async(FrameCodec::default(), &mut stream)
            .await
            .unwrap();
        assert_eq!(hello.expected_instance_nonce, nonce);
        let mut reply = crate::tests::async_server_hello(nonce);
        reply.selected_protocol.as_mut().unwrap().minor = 24;
        write_server_hello_async(FrameCodec::default(), &mut stream, &reply)
            .await
            .unwrap();
        let mut next_byte = [0; 1];
        let read = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut next_byte))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            read, 0,
            "unsupported prefix request must not enter the transport"
        );
    });
    let source = test_source(4, 0, 6);
    let result = client
        .source_read_lossless_text_prefix_async_with_options(
            test_repository(),
            GenerationSelector::Active,
            &[source],
            projection(),
            3,
            RequestOptions::new()
                .with_timeout(RequestTimeout::new(Duration::from_secs(5)).unwrap()),
        )
        .await;
    assert!(matches!(
        result,
        Err(ClientError::ProtocolFeatureUnavailable)
    ));
    server.await.unwrap();
}

#[tokio::test]
async fn lossless_prefix_async_transport_keeps_context_cap_and_single_text_payload() {
    use rootlight_ipc::AsyncLocalListener;
    let (_temporary, endpoint) = crate::tests::async_test_endpoint("prefix-current-minor");
    let listener = AsyncLocalListener::bind(endpoint.clone()).unwrap();
    let nonce = [18; 16];
    let client = Client::new(endpoint, nonce, [19; 16]);
    let source = test_source(4, 0, 12);
    let responses = [
        daemon::SourcePhysicalEncoding::Utf16Le,
        daemon::SourcePhysicalEncoding::Utf16Be,
    ]
    .into_iter()
    .map(|encoding| {
        daemon::response_envelope::Response::SourceRead(wire_prefix(
            &source,
            "\u{feff}a🧭",
            encoding,
            0..8,
            true,
        ))
    })
    .collect();
    let server = tokio::spawn(crate::tests::serve_async_responses(
        listener, nonce, responses,
    ));
    let options = SourceTextReadOptions {
        context_lines_before: 1,
        context_lines_after: 1,
        ..projection()
    };
    for encoding in [SourceTextEncoding::Utf16Le, SourceTextEncoding::Utf16Be] {
        let parsed = client
            .source_read_lossless_text_prefix_async_with_options(
                test_repository(),
                GenerationSelector::Active,
                std::slice::from_ref(&source),
                options,
                8,
                RequestOptions::new()
                    .with_timeout(RequestTimeout::new(Duration::from_secs(5)).unwrap()),
            )
            .await
            .unwrap();
        assert_eq!(parsed.result.chunks[0].source, source);
        assert_eq!(parsed.result.chunks[0].content, "\u{feff}a🧭");
        assert_eq!(parsed.result.chunks[0].physical_encoding, encoding);
        assert!(parsed.prefix_truncated);
        assert!(!parsed.result.truncated);
    }
    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 2);
    for request in requests {
        let Some(daemon::request_envelope::Request::SourceRead(request)) = request.request else {
            panic!("source read request expected");
        };
        assert_eq!(request.maximum_text_bytes_per_chunk, Some(8));
        assert_eq!(
            request.encoding,
            daemon::SourceReadEncoding::LosslessText as i32
        );
        assert_eq!(request.context_lines_before, 1);
        assert_eq!(request.context_lines_after, 1);
        assert!(!request.merge_overlaps);
        assert_eq!(request.include_line_numbers, Some(false));
    }
}

#[tokio::test]
async fn lossless_prefix_invalid_controls_fail_before_connecting() {
    let (_temporary, endpoint) = crate::tests::async_test_endpoint("prefix-invalid-controls");
    let client = Client::new(endpoint, [20; 16], [21; 16]);
    let source = test_source(4, 0, 3);
    let sources = std::slice::from_ref(&source);
    for cap in [0, 512 * 1024 + 1] {
        assert!(matches!(
            client.source_read_lossless_text_prefix_with_options(
                test_repository(),
                GenerationSelector::Active,
                sources,
                projection(),
                cap,
                RequestOptions::new()
            ),
            Err(ClientError::InvalidFirstSliceRequest)
        ));
        assert!(matches!(
            client
                .source_read_lossless_text_prefix_async_with_options(
                    test_repository(),
                    GenerationSelector::Active,
                    sources,
                    projection(),
                    cap,
                    RequestOptions::new()
                )
                .await,
            Err(ClientError::InvalidFirstSliceRequest)
        ));
    }
}
