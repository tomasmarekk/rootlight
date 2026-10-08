//! Lossless source-read boundary tests using the shared immutable-source fixtures.
//! Text presentation must never replace physical identity or legacy byte semantics.

use super::*;
use crate::tests::{
    test_generation, test_repository, test_source, wire_query_context, wire_source_read,
};

fn wire_text(
    source: &SourceReference,
    text: &str,
    encoding: daemon::SourcePhysicalEncoding,
) -> daemon::SourceReadResponse {
    let mut wire = wire_source_read(source);
    let length = source.end_byte - source.start_byte;
    wire.context = Some(wire_query_context(1, length));
    wire.total_source_bytes = length;
    wire.total_text_bytes = u64::try_from(text.len()).unwrap();
    let chunk = &mut wire.chunks[0];
    chunk.content = text.to_owned();
    chunk.encoding = daemon::SourceReadEncoding::LosslessText as i32;
    chunk.physical_encoding = Some(encoding as i32);
    chunk.exact_content.clear();
    wire
}

fn parse(
    wire: daemon::SourceReadResponse,
    references: &[SourceReference],
    projection: SourceTextReadOptions,
) -> Result<SourceTextReadResult, ClientError> {
    parse_source_text_read_for_minor(
        wire,
        test_repository(),
        GenerationSelector::Active,
        references,
        projection,
        CURRENT_PROTOCOL_MINOR,
    )
}

#[test]
fn lossless_source_read_requires_negotiated_minor_without_changing_legacy_modes() {
    let source = test_source(4, 0, 3);
    let references = std::slice::from_ref(&source);
    let request = build_source_text_read_request(
        test_repository(),
        GenerationSelector::Active,
        references,
        SourceTextReadOptions::new(),
    )
    .unwrap();
    assert!(matches!(
        ensure_request_supported(&request, 23),
        Err(ClientError::ProtocolFeatureUnavailable)
    ));
    assert!(ensure_request_supported(&request, 24).is_ok());
    let daemon::request_envelope::Request::SourceRead(request) = request else {
        panic!("source read request expected");
    };
    assert_eq!(
        request.encoding,
        daemon::SourceReadEncoding::LosslessText as i32
    );
    assert_eq!(request.include_line_numbers, Some(true));
    let legacy = build_source_read_request(
        test_repository(),
        GenerationSelector::Active,
        references,
        SourceReadOptions::new(),
    )
    .unwrap();
    assert!(ensure_request_supported(&legacy, 5).is_ok());
    assert!(
        build_source_text_read_request(
            test_repository(),
            GenerationSelector::Active,
            &[],
            SourceTextReadOptions::new()
        )
        .is_err()
    );
    assert!(
        build_source_text_read_request(
            test_repository(),
            GenerationSelector::Active,
            &[source.clone(), source.clone()],
            SourceTextReadOptions::new()
        )
        .is_err()
    );
    assert!(matches!(
        parse_source_text_read_for_minor(
            wire_text(&source, "abc", daemon::SourcePhysicalEncoding::Utf8),
            test_repository(),
            GenerationSelector::Active,
            references,
            SourceTextReadOptions::new(),
            23,
        ),
        Err(ClientError::ProtocolFeatureUnavailable)
    ));
    assert!(
        parse(
            wire_source_read(&source),
            references,
            SourceTextReadOptions::new()
        )
        .is_err()
    );
}

#[test]
fn lossless_source_read_preserves_physical_identity_bom_crlf_and_non_bmp_text() {
    let text = "\u{feff}first\r\n\u{1f9ed} last\r\n";
    for (wire_encoding, encoding, width) in [
        (
            daemon::SourcePhysicalEncoding::Utf8,
            SourceTextEncoding::Utf8,
            text.len(),
        ),
        (
            daemon::SourcePhysicalEncoding::Utf16Le,
            SourceTextEncoding::Utf16Le,
            text.encode_utf16().count() * 2,
        ),
        (
            daemon::SourcePhysicalEncoding::Utf16Be,
            SourceTextEncoding::Utf16Be,
            text.encode_utf16().count() * 2,
        ),
    ] {
        let source = test_source(4, 0, u64::try_from(width).unwrap());
        let wire = wire_text(&source, text, wire_encoding);
        assert!(wire.chunks[0].exact_content.is_empty());
        let parsed = parse(
            wire,
            std::slice::from_ref(&source),
            SourceTextReadOptions::new(),
        )
        .unwrap();
        assert_eq!(parsed.chunks[0].source, source);
        assert_eq!(parsed.chunks[0].content, text);
        assert_eq!(parsed.chunks[0].physical_encoding, encoding);
        assert_eq!(parsed.chunks[0].content_hash, source.content_hash);
        assert_eq!(parsed.chunks[0].start_byte, 0);
        assert_eq!(parsed.chunks[0].end_byte, u64::try_from(width).unwrap());
        assert_eq!(parsed.total_source_bytes, u64::try_from(width).unwrap());
        assert_eq!(parsed.total_text_bytes, u64::try_from(text.len()).unwrap());
        assert_eq!(parsed.context.usage.source_bytes, parsed.total_source_bytes);
        assert!(!format!("{:?}", parsed.chunks[0]).contains("first"));
        assert!(!format!("{parsed:?}").contains("last"));
    }
}

#[test]
fn lossless_source_read_rejects_unpaired_representation_and_corrupt_metadata() {
    let source = test_source(4, 0, 8);
    let good = wire_text(
        &source,
        "\u{feff}abc",
        daemon::SourcePhysicalEncoding::Utf16Le,
    );
    assert!(
        parse(
            good.clone(),
            std::slice::from_ref(&source),
            SourceTextReadOptions::new()
        )
        .is_ok()
    );
    let mut invalid = Vec::new();
    let mut wire = good.clone();
    wire.chunks[0].physical_encoding = None;
    invalid.push(wire);
    let mut wire = good.clone();
    wire.chunks[0].physical_encoding = Some(99);
    invalid.push(wire);
    let mut wire = good.clone();
    wire.chunks[0].physical_encoding = Some(daemon::SourcePhysicalEncoding::Utf8 as i32);
    invalid.push(wire);
    let mut wire = good.clone();
    wire.chunks[0].exact_content = b"duplicate".to_vec();
    invalid.push(wire);
    let mut wire = good.clone();
    wire.chunks[0].content.push('x');
    invalid.push(wire);
    let mut wire = good.clone();
    wire.total_text_bytes += 1;
    invalid.push(wire);
    let mut wire = good.clone();
    wire.total_source_bytes += 1;
    invalid.push(wire);
    let mut wire = good.clone();
    wire.context
        .as_mut()
        .unwrap()
        .usage
        .as_mut()
        .unwrap()
        .source_bytes += 1;
    invalid.push(wire);
    let mut wire = good.clone();
    wire.context
        .as_mut()
        .unwrap()
        .usage
        .as_mut()
        .unwrap()
        .results = 2;
    invalid.push(wire);
    let mut wire = good.clone();
    wire.chunks[0].source.as_mut().unwrap().generation =
        Some(generation_to_wire(GenerationId::from_bytes([9; 20])));
    invalid.push(wire);
    let mut wire = good.clone();
    wire.chunks[0].content_hash = Some(content_hash_to_wire(ContentHash::from_bytes([9; 32])));
    invalid.push(wire);
    let mut wire = good.clone();
    wire.chunks[0].source = Some(source_reference_to_wire(&test_source(5, 0, 8)));
    invalid.push(wire);
    let mut wire = good.clone();
    wire.chunks[0].included_start_line = None;
    invalid.push(wire);
    let mut wire = good.clone();
    wire.chunks[0].start_line = 2;
    invalid.push(wire);
    for (index, wire) in invalid.into_iter().enumerate() {
        assert!(
            matches!(
                parse(
                    wire,
                    std::slice::from_ref(&source),
                    SourceTextReadOptions::new()
                ),
                Err(ClientError::InvalidResponseCorrelation)
            ),
            "tamper case {index}"
        );
    }
    let legacy_source = test_source(4, 0, 3);
    let legacy = wire_source_read(&legacy_source);
    assert!(
        parse_source_read(
            legacy.clone(),
            test_repository(),
            GenerationSelector::Active,
            std::slice::from_ref(&legacy_source),
            SourceReadOptions::new()
        )
        .is_ok()
    );
    let mut marker = legacy.clone();
    marker.chunks[0].physical_encoding = Some(daemon::SourcePhysicalEncoding::Utf8 as i32);
    let mut text_total = legacy;
    text_total.total_text_bytes = 3;
    for wire in [marker, text_total] {
        assert!(
            parse_source_read(
                wire,
                test_repository(),
                GenerationSelector::Active,
                std::slice::from_ref(&legacy_source),
                SourceReadOptions::new()
            )
            .is_err()
        );
    }
}

#[test]
fn lossless_source_read_checks_selected_scalar_endpoints_inside_expanded_ranges() {
    let projection = SourceTextReadOptions {
        context_lines_before: 1,
        context_lines_after: 1,
        ..SourceTextReadOptions::new()
    };
    for (encoding, text, width, valid, invalid) in [
        (
            daemon::SourcePhysicalEncoding::Utf8,
            "a\u{1f9ed}z",
            6,
            1..5,
            2..5,
        ),
        (
            daemon::SourcePhysicalEncoding::Utf16Le,
            "\u{feff}a\u{1f9ed}z",
            10,
            4..8,
            6..8,
        ),
        (
            daemon::SourcePhysicalEncoding::Utf16Be,
            "\u{feff}a\u{1f9ed}z",
            10,
            4..8,
            4..6,
        ),
    ] {
        let good = test_source(4, valid.start, valid.end);
        let bad = test_source(4, invalid.start, invalid.end);
        let expanded = |source: &SourceReference| {
            let mut wire = wire_text(source, text, encoding);
            wire.chunks[0].start_byte = 0;
            wire.chunks[0].end_byte = width;
            wire.context = Some(wire_query_context(1, width));
            wire.total_source_bytes = width;
            wire
        };
        let parsed = parse(expanded(&good), std::slice::from_ref(&good), projection).unwrap();
        assert_eq!(parsed.chunks[0].source, good);
        assert_eq!(parsed.chunks[0].start_byte, 0);
        assert_eq!(parsed.chunks[0].end_byte, width);
        assert!(parse(expanded(&bad), std::slice::from_ref(&bad), projection).is_err());
        if encoding != daemon::SourcePhysicalEncoding::Utf8 {
            let odd = test_source(4, 1, 6);
            assert!(parse(expanded(&odd), std::slice::from_ref(&odd), projection).is_err());
        }
        assert!(
            parse(
                expanded(&good),
                std::slice::from_ref(&good),
                SourceTextReadOptions::new()
            )
            .is_err()
        );
    }
}

#[test]
fn lossless_source_read_preserves_empty_and_bom_only_chunks_without_duplicate_storage() {
    for (text, width, encoding) in [
        ("", 0, daemon::SourcePhysicalEncoding::Utf8),
        ("", 0, daemon::SourcePhysicalEncoding::Utf16Le),
        ("", 0, daemon::SourcePhysicalEncoding::Utf16Be),
        ("\u{feff}", 2, daemon::SourcePhysicalEncoding::Utf16Le),
        ("\u{feff}", 2, daemon::SourcePhysicalEncoding::Utf16Be),
    ] {
        let source = test_source(4, 0, width);
        let mut wire = wire_text(&source, text, encoding);
        let projection = SourceTextReadOptions {
            include_line_numbers: false,
            ..SourceTextReadOptions::new()
        };
        wire.chunks[0].included_start_line = None;
        wire.chunks[0].included_end_line = None;
        wire.chunks[0].start_line = 0;
        wire.chunks[0].end_line = 0;
        let parsed = parse(wire, std::slice::from_ref(&source), projection).unwrap();
        assert_eq!(parsed.chunks[0].content, text);
        assert_eq!(parsed.chunks[0].source, source);
        assert_eq!(parsed.total_source_bytes, width);
        assert_eq!(parsed.total_text_bytes, u64::try_from(text.len()).unwrap());
        assert_eq!(parsed.chunks[0].start_line, None);
    }
}

#[test]
fn lossless_source_read_requires_bom_only_at_nonempty_utf16_file_start() {
    for encoding in [
        daemon::SourcePhysicalEncoding::Utf16Le,
        daemon::SourcePhysicalEncoding::Utf16Be,
    ] {
        let beginning = test_source(4, 0, 4);
        assert!(matches!(
            parse(
                wire_text(&beginning, "ab", encoding),
                std::slice::from_ref(&beginning),
                SourceTextReadOptions::new()
            ),
            Err(ClientError::InvalidResponseCorrelation)
        ));
        let interior = test_source(4, 2, 6);
        let parsed = parse(
            wire_text(&interior, "ab", encoding),
            std::slice::from_ref(&interior),
            SourceTextReadOptions::new(),
        )
        .unwrap();
        assert_eq!(parsed.chunks[0].source, interior);
        assert_eq!(parsed.chunks[0].content, "ab");
        let marked = test_source(4, 0, 6);
        assert!(
            parse(
                wire_text(&marked, "\u{feff}ab", encoding),
                std::slice::from_ref(&marked),
                SourceTextReadOptions::new()
            )
            .is_ok()
        );
    }
    let utf8 = test_source(4, 0, 2);
    assert!(
        parse(
            wire_text(&utf8, "ab", daemon::SourcePhysicalEncoding::Utf8),
            std::slice::from_ref(&utf8),
            SourceTextReadOptions::new()
        )
        .is_ok()
    );
}

#[test]
fn lossless_source_read_included_lines_must_contain_selected_line_hints() {
    for encoding in [
        daemon::SourcePhysicalEncoding::Utf16Le,
        daemon::SourcePhysicalEncoding::Utf16Be,
    ] {
        let full = test_source(4, 0, 6);
        let mut forged = wire_text(&full, "\u{feff}ab", encoding);
        forged.chunks[0].included_start_line = Some(2);
        forged.chunks[0].included_end_line = Some(2);
        forged.chunks[0].start_line = 2;
        forged.chunks[0].end_line = 2;
        assert!(matches!(
            parse(
                forged,
                std::slice::from_ref(&full),
                SourceTextReadOptions::new()
            ),
            Err(ClientError::InvalidResponseCorrelation)
        ));
        let selected = SourceReference::new(
            test_repository(),
            test_generation(),
            FileId::from_bytes([4; 20]),
            8..10,
            ContentHash::from_bytes([4; 32]),
            Some(2..=2),
        )
        .unwrap();
        let mut expanded = wire_text(&selected, "\u{feff}a\r\nb\r\nc", encoding);
        expanded.context = Some(wire_query_context(1, 16));
        expanded.total_source_bytes = 16;
        expanded.chunks[0].start_byte = 0;
        expanded.chunks[0].end_byte = 16;
        expanded.chunks[0].included_start_line = Some(1);
        expanded.chunks[0].included_end_line = Some(3);
        expanded.chunks[0].start_line = 1;
        expanded.chunks[0].end_line = 3;
        let projection = SourceTextReadOptions {
            context_lines_before: 1,
            context_lines_after: 1,
            ..SourceTextReadOptions::new()
        };
        let parsed = parse(
            expanded.clone(),
            std::slice::from_ref(&selected),
            projection,
        )
        .unwrap();
        assert_eq!(parsed.chunks[0].source, selected);
        assert_eq!(parsed.chunks[0].start_line, Some(1));
        assert_eq!(parsed.chunks[0].end_line, Some(3));
        let mut too_short = expanded.clone();
        too_short.chunks[0].included_end_line = Some(1);
        too_short.chunks[0].end_line = 1;
        assert!(parse(too_short, std::slice::from_ref(&selected), projection).is_err());
        expanded.chunks[0].included_start_line = None;
        expanded.chunks[0].included_end_line = None;
        expanded.chunks[0].start_line = 0;
        expanded.chunks[0].end_line = 0;
        assert!(
            parse(
                expanded,
                std::slice::from_ref(&selected),
                SourceTextReadOptions {
                    include_line_numbers: false,
                    ..projection
                }
            )
            .is_ok()
        );
    }
}

#[test]
fn lossless_source_read_rejects_utf16_nul_without_changing_utf8_policy() {
    for encoding in [
        daemon::SourcePhysicalEncoding::Utf16Le,
        daemon::SourcePhysicalEncoding::Utf16Be,
    ] {
        let source = test_source(4, 2, 8);
        assert!(
            parse(
                wire_text(&source, "a\0b", encoding),
                std::slice::from_ref(&source),
                SourceTextReadOptions::new()
            )
            .is_err()
        );
    }
    let source = test_source(4, 0, 3);
    let parsed = parse(
        wire_text(&source, "a\0b", daemon::SourcePhysicalEncoding::Utf8),
        std::slice::from_ref(&source),
        SourceTextReadOptions::new(),
    )
    .unwrap();
    assert_eq!(parsed.chunks[0].content.as_bytes(), b"a\0b");
}

#[test]
fn lossless_source_read_preserves_container_origin_and_assesses_physical_expansion() {
    let selected = test_source(4, 4, 8);
    let expanded = test_source(4, 0, 10);
    let origin = SourceReference::new(
        test_repository(),
        test_generation(),
        FileId::from_bytes([7; 20]),
        0..4096,
        ContentHash::from_bytes([7; 32]),
        None,
    )
    .unwrap();
    let projection = SourceTextReadOptions {
        context_lines_before: 1,
        context_lines_after: 1,
        ..SourceTextReadOptions::new()
    };
    let mut wire = wire_text(
        &selected,
        "\u{feff}a\u{1f9ed}z",
        daemon::SourcePhysicalEncoding::Utf16Le,
    );
    wire.context = Some(wire_query_context(1, 10));
    wire.total_source_bytes = 10;
    wire.chunks[0].start_byte = 0;
    wire.chunks[0].end_byte = 10;
    wire.chunks[0].language = "vba".to_owned();
    wire.chunks[0].extracted_from = Some(source_reference_to_wire(&origin));
    wire.conditional_activity = vec![crate::tests::wire_activity(
        &expanded,
        daemon::ConditionalActivityBasis::ReturnedRange,
    )];
    let parsed = parse(wire.clone(), std::slice::from_ref(&selected), projection).unwrap();
    assert_eq!(parsed.chunks[0].extracted_from, Some(origin.clone()));
    assert_eq!(parsed.chunks[0].source, selected);
    assert_eq!(
        parsed.conditional_activity[0].assessed_source_ref,
        Some(expanded)
    );
    let mut wrong_origin = wire.clone();
    wrong_origin.chunks[0]
        .extracted_from
        .as_mut()
        .unwrap()
        .start_byte = 2;
    assert!(parse(wrong_origin, std::slice::from_ref(&selected), projection).is_err());
    let mut wrong_origin = wire.clone();
    wrong_origin.chunks[0]
        .extracted_from
        .as_mut()
        .unwrap()
        .generation = Some(generation_to_wire(GenerationId::from_bytes([9; 20])));
    assert!(parse(wrong_origin, std::slice::from_ref(&selected), projection).is_err());
    let mut wrong_assessment = wire.clone();
    wrong_assessment.conditional_activity[0].assessed_source_ref =
        Some(source_reference_to_wire(&selected));
    assert!(
        parse(
            wrong_assessment,
            std::slice::from_ref(&selected),
            projection
        )
        .is_err()
    );
    let mut wrong_basis = wire.clone();
    wrong_basis.conditional_activity[0].basis =
        daemon::ConditionalActivityBasis::DefinitionToken as i32;
    assert!(parse(wrong_basis, std::slice::from_ref(&selected), projection).is_err());
    wire.conditional_activity.clear();
    assert!(parse(wire, std::slice::from_ref(&selected), projection).is_err());
}

#[test]
fn lossless_source_read_conserves_merged_totals_and_ordered_truncated_prefixes() {
    let left = test_source(4, 0, 4);
    let right = test_source(4, 2, 8);
    let merged = merge_client_source_references(&[left.clone(), right.clone()]).unwrap();
    let projection = SourceTextReadOptions {
        merge_overlaps: true,
        ..SourceTextReadOptions::new()
    };
    let wire = wire_text(
        &merged[0],
        "\u{feff}abc",
        daemon::SourcePhysicalEncoding::Utf16Be,
    );
    let parsed = parse(wire, &[left, right], projection).unwrap();
    assert_eq!(parsed.chunks[0].source, merged[0]);
    assert_eq!(parsed.total_source_bytes, 8);
    assert_eq!(parsed.total_text_bytes, 6);
    let first = test_source(4, 2, 4);
    let second = test_source(5, 2, 6);
    let mut wire = wire_text(&first, "a", daemon::SourcePhysicalEncoding::Utf16Le);
    let second_wire = wire_text(&second, "bc", daemon::SourcePhysicalEncoding::Utf16Be);
    wire.chunks.extend(second_wire.chunks);
    wire.context = Some(wire_query_context(2, 6));
    wire.total_source_bytes = 6;
    wire.total_text_bytes = 3;
    let refs = [first, second];
    assert!(parse(wire.clone(), &refs, SourceTextReadOptions::new()).is_ok());
    let mut reordered = wire.clone();
    reordered.chunks.swap(0, 1);
    assert!(parse(reordered, &refs, SourceTextReadOptions::new()).is_err());
    wire.chunks.pop();
    wire.context = Some(wire_query_context(1, 2));
    wire.total_source_bytes = 2;
    wire.total_text_bytes = 1;
    assert!(parse(wire.clone(), &refs, SourceTextReadOptions::new()).is_err());
    wire.truncated = true;
    let parsed = parse(wire, &refs, SourceTextReadOptions::new()).unwrap();
    assert!(parsed.truncated);
    assert_eq!(parsed.chunks.len(), 1);
}

#[tokio::test]
async fn lossless_source_read_older_minor_closes_before_writing_an_operation() {
    use rootlight_ipc::{AsyncLocalListener, read_client_hello_async, write_server_hello_async};
    use tokio::io::AsyncReadExt as _;
    let (_temporary, endpoint) = crate::tests::async_test_endpoint("lossless-old-minor");
    let listener = AsyncLocalListener::bind(endpoint.clone()).unwrap();
    let nonce = [12; 16];
    let client = Client::new(endpoint, nonce, [13; 16]);
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
        reply.selected_protocol.as_mut().unwrap().minor = 23;
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
            "unsupported operation must not enter the transport"
        );
    });
    let source = test_source(4, 0, 6);
    let result = client
        .source_read_lossless_text_async_with_options(
            test_repository(),
            GenerationSelector::Active,
            &[source],
            SourceTextReadOptions::new(),
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
async fn lossless_source_read_async_transport_retains_controls_and_single_text_payload() {
    use rootlight_ipc::AsyncLocalListener;
    let (_temporary, endpoint) = crate::tests::async_test_endpoint("lossless-current-minor");
    let listener = AsyncLocalListener::bind(endpoint.clone()).unwrap();
    let nonce = [14; 16];
    let client = Client::new(endpoint, nonce, [15; 16]);
    let source = test_source(4, 0, 10);
    let wire = wire_text(
        &source,
        "\u{feff}a\u{1f9ed}z",
        daemon::SourcePhysicalEncoding::Utf16Be,
    );
    let server = tokio::spawn(crate::tests::serve_async_responses(
        listener,
        nonce,
        vec![daemon::response_envelope::Response::SourceRead(wire)],
    ));
    let projection = SourceTextReadOptions {
        context_lines_before: 2,
        context_lines_after: 3,
        merge_overlaps: true,
        ..SourceTextReadOptions::new()
    };
    let parsed = client
        .source_read_lossless_text_async_with_options(
            test_repository(),
            GenerationSelector::Active,
            std::slice::from_ref(&source),
            projection,
            RequestOptions::new()
                .with_timeout(RequestTimeout::new(Duration::from_secs(5)).unwrap()),
        )
        .await
        .unwrap();
    assert_eq!(parsed.chunks[0].content, "\u{feff}a\u{1f9ed}z");
    assert_eq!(parsed.chunks[0].source, source);
    let requests = server.await.unwrap();
    let Some(daemon::request_envelope::Request::SourceRead(request)) = requests[0].request.as_ref()
    else {
        panic!("source read request expected");
    };
    assert_eq!(
        request.encoding,
        daemon::SourceReadEncoding::LosslessText as i32
    );
    assert_eq!(request.context_lines_before, 2);
    assert_eq!(request.context_lines_after, 3);
    assert!(request.merge_overlaps);
}
