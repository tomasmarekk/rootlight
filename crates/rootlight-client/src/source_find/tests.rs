//! Literal response admission tests bind wire metadata to the original request.
//! Authored pages exercise overlaps and Unicode physical coordinates without
//! substituting client-side source reads for the daemon's immutable-source proof.

use super::*;
use crate::tests::{test_generation, test_repository};

fn source(start: u64, end: u64) -> SourceReference {
    SourceReference::new(
        test_repository(),
        test_generation(),
        FileId::from_bytes([4; 20]),
        start..end,
        ContentHash::from_bytes([5; 32]),
        None,
    )
    .unwrap()
}

fn fixture() -> daemon::SourceFindResponse {
    daemon::SourceFindResponse {
        schema_version: Some(first_slice_schema()),
        repository: Some(repository_to_wire(test_repository())),
        generation: Some(generation_to_wire(test_generation())),
        file: Some(daemon::SourceFindFile {
            file: Some(file_to_wire(source(0, 0).file)),
            path: "src/written.v".to_owned(),
            language: "v".to_owned(),
            content_hash: Some(common::ContentHash {
                value: source(0, 0).content_hash.as_bytes().to_vec(),
            }),
            byte_length: 8,
            encoding: daemon::SourcePhysicalEncoding::Utf8 as i32,
            generated: false,
            extracted_from: None,
        }),
        matches: [0..3, 2..5, 4..7]
            .into_iter()
            .map(|span| source_reference_to_wire(&source(span.start, span.end)))
            .collect(),
        scanned_start_byte: 0,
        scanned_end_byte: 8,
        restoration_bytes: 0,
        stop: daemon::SourceFindStop::EndOfSource as i32,
        continuation: None,
        total_metadata_bytes: 14,
        total_response_memory_bytes: 1024,
    }
}

fn parse(
    wire: daemon::SourceFindResponse,
    literal: &str,
    search: SourceFindOptions,
) -> Result<SourceFindResult, ClientError> {
    parse_response(
        wire,
        test_repository(),
        GenerationSelector::Generation(test_generation()),
        source(0, 0).file,
        literal,
        search,
        RequestOptions::new(),
        SOURCE_FIND_PROTOCOL_MINOR,
    )
}

fn effective(results: u64, memory: u64) -> RequestOptions {
    RequestOptions::new().with_effective_budget(
        EffectiveBudget::new(EffectiveBudgetLimits {
            rows: 100,
            edges: 100,
            results,
            source_bytes: 1024,
            json_bytes: 4096,
            estimated_tokens: 4096,
            memory_bytes: memory,
            duration: Duration::from_secs(1),
            depth: None,
            paths: None,
        })
        .unwrap(),
    )
}

#[test]
fn literal_request_requires_bounded_controls_and_negotiated_capability() {
    let request = build_request(
        test_repository(),
        GenerationSelector::Active,
        source(0, 0).file,
        "literal",
        SourceFindOptions::new(),
    )
    .unwrap();
    assert!(CLIENT_CAPABILITIES.contains(&"source.find.v1"));
    assert!(ensure_request_supported(&request, 26).is_err());
    assert!(ensure_request_supported(&request, 27).is_ok());
    assert!(ensure_request_capability(&request, &[]).is_err());
    assert!(ensure_request_capability(&request, &["source.find.v1".to_owned()]).is_ok());
    for controls in [
        SourceFindOptions {
            maximum_scan_bytes: 0,
            ..SourceFindOptions::new()
        },
        SourceFindOptions {
            maximum_scan_bytes: MAX_SOURCE_FIND_SCAN_BYTES + 1,
            ..SourceFindOptions::new()
        },
        SourceFindOptions {
            maximum_matches: 0,
            ..SourceFindOptions::new()
        },
        SourceFindOptions {
            maximum_matches: MAX_SOURCE_FIND_MATCHES + 1,
            ..SourceFindOptions::new()
        },
        SourceFindOptions {
            cursor: Some(SourceFindCursor {
                next_byte: 3,
                matched_prefix: 7,
            }),
            ..SourceFindOptions::new()
        },
    ] {
        assert!(
            build_request(
                test_repository(),
                GenerationSelector::Generation(test_generation()),
                source(0, 0).file,
                "literal",
                controls
            )
            .is_err()
        );
    }
    for literal in [String::new(), "x".repeat(MAX_SOURCE_FIND_LITERAL_BYTES + 1)] {
        assert!(
            build_request(
                test_repository(),
                GenerationSelector::Active,
                source(0, 0).file,
                &literal,
                SourceFindOptions::new()
            )
            .is_err()
        );
    }
    let resumed = SourceFindOptions {
        cursor: Some(SourceFindCursor {
            next_byte: 4,
            matched_prefix: 2,
        }),
        ..SourceFindOptions::new()
    };
    assert!(
        build_request(
            test_repository(),
            GenerationSelector::Active,
            source(0, 0).file,
            "aba",
            resumed
        )
        .is_err()
    );
    assert!(
        build_request(
            test_repository(),
            GenerationSelector::Generation(test_generation()),
            source(0, 0).file,
            "aba",
            resumed
        )
        .is_ok()
    );
}

#[test]
fn literal_pages_preserve_overlaps_and_cross_page_partial_matches() {
    let parsed = parse(fixture(), "aba", SourceFindOptions::new()).unwrap();
    assert_eq!(
        parsed.matches,
        vec![source(0, 3), source(2, 5), source(4, 7)]
    );
    assert_eq!(parsed.scanned, 0..8);
    assert_eq!(parsed.stop, SourceFindStop::EndOfSource);
    let mut first = fixture();
    first.matches.truncate(1);
    first.scanned_end_byte = 4;
    first.stop = daemon::SourceFindStop::ScanLimit as i32;
    first.continuation = Some(daemon::SourceFindCursor {
        next_byte: 4,
        matched_prefix: 2,
    });
    let controls = SourceFindOptions {
        maximum_scan_bytes: 4,
        ..SourceFindOptions::new()
    };
    let page = parse(first, "aba", controls).unwrap();
    let mut second = fixture();
    second.matches.remove(0);
    second.scanned_start_byte = 4;
    second.restoration_bytes = 3;
    let next = parse(
        second.clone(),
        "aba",
        SourceFindOptions {
            cursor: page.continuation,
            ..controls
        },
    )
    .unwrap();
    assert_eq!(next.matches, vec![source(2, 5), source(4, 7)]);
    second.restoration_bytes = 1;
    assert!(
        parse(
            second,
            "aba",
            SourceFindOptions {
                cursor: page.continuation,
                ..controls
            }
        )
        .is_err()
    );
}

#[test]
fn literal_pages_validate_utf16_physical_widths_and_bom_boundaries() {
    for encoding in [
        daemon::SourcePhysicalEncoding::Utf16Le,
        daemon::SourcePhysicalEncoding::Utf16Be,
    ] {
        let mut wire = fixture();
        let file = wire.file.as_mut().unwrap();
        file.encoding = encoding as i32;
        file.byte_length = 14;
        wire.scanned_start_byte = 2;
        wire.scanned_end_byte = 14;
        wire.matches = vec![
            source_reference_to_wire(&source(2, 8)),
            source_reference_to_wire(&source(8, 14)),
        ];
        let parsed = parse(wire.clone(), "a🧭", SourceFindOptions::new()).unwrap();
        assert_eq!(parsed.matches, vec![source(2, 8), source(8, 14)]);
        let mut odd = wire.clone();
        odd.matches[0].start_byte = 3;
        odd.matches[0].end_byte = 9;
        assert!(parse(odd, "a🧭", SourceFindOptions::new()).is_err());
        let mut bom_match = wire.clone();
        bom_match.matches[0].start_byte = 0;
        bom_match.matches[0].end_byte = 6;
        assert!(parse(bom_match, "a🧭", SourceFindOptions::new()).is_err());
        wire.file.as_mut().unwrap().byte_length = 15;
        wire.scanned_end_byte = 15;
        assert!(parse(wire, "a🧭", SourceFindOptions::new()).is_err());
    }
    for (encoding, start) in [
        (daemon::SourcePhysicalEncoding::Utf8, 0),
        (daemon::SourcePhysicalEncoding::Utf16Le, 2),
    ] {
        let mut empty = fixture();
        empty.file.as_mut().unwrap().encoding = encoding as i32;
        empty.file.as_mut().unwrap().byte_length = start;
        empty.scanned_start_byte = start;
        empty.scanned_end_byte = start;
        empty.matches.clear();
        assert!(
            parse(empty, "needle", SourceFindOptions::new())
                .unwrap()
                .matches
                .is_empty()
        );
    }
    let mut bom = fixture();
    bom.file.as_mut().unwrap().byte_length = 3;
    bom.scanned_end_byte = 3;
    bom.matches.truncate(1);
    assert_eq!(
        parse(bom, "\u{feff}", SourceFindOptions::new())
            .unwrap()
            .matches,
        vec![source(0, 3)]
    );
}

#[test]
fn literal_pages_reject_uncorrelated_sources_and_inconsistent_accounting() {
    let mutations: Vec<fn(&mut daemon::SourceFindResponse)> = vec![
        |wire| wire.repository = Some(repository_to_wire(RepositoryId::from_bytes([9; 16]))),
        |wire| wire.generation = Some(generation_to_wire(GenerationId::from_bytes([9; 20]))),
        |wire| wire.file = None,
        |wire| wire.file.as_mut().unwrap().file = Some(file_to_wire(FileId::from_bytes([9; 20]))),
        |wire| wire.file.as_mut().unwrap().path = "../outside.v".to_owned(),
        |wire| wire.file.as_mut().unwrap().path = "C:/outside.v".to_owned(),
        |wire| wire.file.as_mut().unwrap().language = "V code".to_owned(),
        |wire| wire.file.as_mut().unwrap().encoding = 999,
        |wire| wire.matches[0].file = Some(file_to_wire(FileId::from_bytes([9; 20]))),
        |wire| wire.matches[0].content_hash.as_mut().unwrap().value[0] ^= 1,
        |wire| wire.matches[0].start_line = Some(1),
        |wire| wire.matches[0].end_byte = 4,
        |wire| wire.matches.swap(0, 1),
        |wire| wire.matches.push(wire.matches[2].clone()),
        |wire| wire.scanned_start_byte = 1,
        |wire| wire.scanned_end_byte = 7,
        |wire| wire.restoration_bytes = 1,
        |wire| wire.total_metadata_bytes += 1,
        |wire| wire.total_response_memory_bytes = 0,
        |wire| wire.stop = 999,
        |wire| {
            wire.continuation = Some(daemon::SourceFindCursor {
                next_byte: 8,
                matched_prefix: 0,
            })
        },
    ];
    for (index, mutate) in mutations.into_iter().enumerate() {
        let mut wire = fixture();
        mutate(&mut wire);
        assert!(
            parse(wire, "aba", SourceFindOptions::new()).is_err(),
            "mutation {index}"
        );
    }
    let mut extracted = fixture();
    extracted.file.as_mut().unwrap().extracted_from = Some(source_reference_to_wire(
        &SourceReference::new(
            test_repository(),
            test_generation(),
            FileId::from_bytes([8; 20]),
            0..100,
            ContentHash::from_bytes([8; 32]),
            None,
        )
        .unwrap(),
    ));
    assert!(
        parse(extracted.clone(), "aba", SourceFindOptions::new())
            .unwrap()
            .file
            .extracted_from
            .is_some()
    );
    let mut hinted = extracted.clone();
    hinted.matches[0].start_line = Some(1);
    hinted.matches[0].end_line = Some(1);
    let origin = hinted
        .file
        .as_mut()
        .unwrap()
        .extracted_from
        .as_mut()
        .unwrap();
    origin.start_line = Some(1);
    origin.end_line = Some(10);
    let result = parse(hinted.clone(), "aba", SourceFindOptions::new()).unwrap();
    assert_eq!(result.matches[0].start_line, Some(1));
    assert_eq!(result.file.extracted_from.unwrap().end_line, Some(10));
    for (start, end) in [(Some(0), Some(1)), (Some(2), Some(1)), (None, Some(1))] {
        let mut malformed = hinted.clone();
        let origin = malformed
            .file
            .as_mut()
            .unwrap()
            .extracted_from
            .as_mut()
            .unwrap();
        origin.start_line = start;
        origin.end_line = end;
        assert!(parse(malformed, "aba", SourceFindOptions::new()).is_err());
    }
    let mut empty_origin = extracted.clone();
    empty_origin
        .file
        .as_mut()
        .unwrap()
        .extracted_from
        .as_mut()
        .unwrap()
        .end_byte = 0;
    assert!(parse(empty_origin, "aba", SourceFindOptions::new()).is_err());
    extracted
        .file
        .as_mut()
        .unwrap()
        .extracted_from
        .as_mut()
        .unwrap()
        .start_byte = 1;
    assert!(parse(extracted, "aba", SourceFindOptions::new()).is_err());
}

#[test]
fn literal_pages_require_progress_and_truthful_stop_causes() {
    let mut limited = fixture();
    limited.matches.truncate(1);
    limited.scanned_end_byte = 3;
    limited.stop = daemon::SourceFindStop::MatchLimit as i32;
    limited.continuation = Some(daemon::SourceFindCursor {
        next_byte: 3,
        matched_prefix: 1,
    });
    let controls = SourceFindOptions {
        maximum_matches: 1,
        ..SourceFindOptions::new()
    };
    assert!(parse(limited.clone(), "aba", controls).is_ok());
    assert!(parse(limited.clone(), "aba", SourceFindOptions::new()).is_err());
    for stop in [
        daemon::SourceFindStop::MatcherMemoryLimit,
        daemon::SourceFindStop::ResponseMemoryLimit,
    ] {
        let mut wire = limited.clone();
        wire.stop = stop as i32;
        assert!(parse(wire.clone(), "aba", SourceFindOptions::new()).is_ok());
        assert!(parse(wire, "aba", controls).is_err());
    }
    for cursor in [
        None,
        Some(daemon::SourceFindCursor {
            next_byte: 2,
            matched_prefix: 1,
        }),
        Some(daemon::SourceFindCursor {
            next_byte: 3,
            matched_prefix: 3,
        }),
    ] {
        let mut wire = limited.clone();
        wire.continuation = cursor;
        assert!(parse(wire, "aba", controls).is_err());
    }
    let mut no_progress = limited;
    no_progress.scanned_end_byte = 0;
    no_progress.matches.clear();
    no_progress.continuation.as_mut().unwrap().next_byte = 0;
    assert!(parse(no_progress, "aba", controls).is_err());
}

#[test]
fn literal_transport_result_and_memory_limits_remain_distinct() {
    let mut wire = fixture();
    wire.matches.truncate(1);
    wire.scanned_end_byte = 3;
    wire.stop = daemon::SourceFindStop::ResultLimit as i32;
    wire.continuation = Some(daemon::SourceFindCursor {
        next_byte: 3,
        matched_prefix: 1,
    });
    let run = |wire, options| {
        parse_response(
            wire,
            test_repository(),
            GenerationSelector::Active,
            source(0, 0).file,
            "aba",
            SourceFindOptions::new(),
            options,
            27,
        )
    };
    assert_eq!(
        run(wire.clone(), effective(1, 4096)).unwrap().stop,
        SourceFindStop::ResultLimit
    );
    assert!(run(wire.clone(), RequestOptions::new()).is_err());
    assert!(run(wire.clone(), effective(2, 4096)).is_err());
    let mut row_limits = effective(2, 4096).effective_budget().unwrap().limits();
    row_limits.rows = 2;
    assert!(
        run(
            wire.clone(),
            RequestOptions::new().with_effective_budget(EffectiveBudget::new(row_limits).unwrap())
        )
        .is_ok()
    );
    assert!(run(wire.clone(), effective(1, 128)).is_err());
    wire.stop = daemon::SourceFindStop::MatchLimit as i32;
    assert!(run(wire, effective(1, 4096)).is_err());
}

#[test]
fn literal_zero_result_capacity_cannot_claim_empty_eof() {
    let mut wire = fixture();
    wire.file.as_mut().unwrap().byte_length = 0;
    wire.matches.clear();
    wire.scanned_end_byte = 0;
    let mut limits = effective(1, 4096).effective_budget().unwrap().limits();
    for rows in [2, 1] {
        limits.rows = rows;
        let result = parse_response(
            wire.clone(),
            test_repository(),
            GenerationSelector::Active,
            source(0, 0).file,
            "aba",
            SourceFindOptions::new(),
            RequestOptions::new().with_effective_budget(EffectiveBudget::new(limits).unwrap()),
            SOURCE_FIND_PROTOCOL_MINOR,
        );
        assert_eq!(
            result.is_ok(),
            rows == 2,
            "one metadata row leaves no match credit"
        );
    }
}

#[tokio::test]
async fn literal_async_transport_preserves_controls_and_source_free_matches() {
    use rootlight_ipc::AsyncLocalListener;
    let (_temporary, endpoint) = crate::tests::async_test_endpoint("source-find");
    let listener = AsyncLocalListener::bind(endpoint.clone()).unwrap();
    let nonce = [41; 16];
    let client = Client::new(endpoint, nonce, [42; 16]);
    let server = tokio::spawn(crate::tests::serve_async_responses(
        listener,
        nonce,
        vec![daemon::response_envelope::Response::SourceFind(fixture())],
    ));
    let controls = SourceFindOptions {
        maximum_scan_bytes: 8,
        maximum_matches: 4,
        cursor: None,
    };
    let result = client
        .source_find_async_with_options(
            test_repository(),
            GenerationSelector::Active,
            source(0, 0).file,
            "aba",
            controls,
            RequestOptions::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        result.matches,
        vec![source(0, 3), source(2, 5), source(4, 7)]
    );
    let requests = server.await.unwrap();
    let Some(daemon::request_envelope::Request::SourceFind(request)) = requests[0].request.as_ref()
    else {
        panic!("source-find request expected")
    };
    assert_eq!(request.literal, "aba");
    assert_eq!(request.maximum_scan_bytes, 8);
    assert_eq!(request.maximum_matches, 4);
    assert!(!format!("{result:?}").contains("src/written.v"));
}

#[tokio::test]
async fn literal_unavailable_protocol_or_capability_closes_before_request() {
    use rootlight_ipc::{AsyncLocalListener, read_client_hello_async, write_server_hello_async};
    use tokio::io::AsyncReadExt as _;
    for old_minor in [true, false] {
        let (_temporary, endpoint) = crate::tests::async_test_endpoint("source-find-negotiation");
        let listener = AsyncLocalListener::bind(endpoint.clone()).unwrap();
        let nonce = [43; 16];
        let client = Client::new(endpoint, nonce, [44; 16]);
        let server = tokio::spawn(async move {
            let mut stream = listener
                .accept_timeout(Duration::from_secs(2))
                .await
                .unwrap();
            read_client_hello_async(FrameCodec::default(), &mut stream)
                .await
                .unwrap();
            let mut hello = crate::tests::async_server_hello(nonce);
            if old_minor {
                hello.selected_protocol.as_mut().unwrap().minor = 26;
            } else {
                hello
                    .capabilities
                    .retain(|capability| capability != "source.find.v1");
            }
            write_server_hello_async(FrameCodec::default(), &mut stream, &hello)
                .await
                .unwrap();
            let mut next = [0; 1];
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(2), stream.read(&mut next))
                    .await
                    .unwrap()
                    .unwrap(),
                0
            );
        });
        let result = client
            .source_find_async_with_options(
                test_repository(),
                GenerationSelector::Active,
                source(0, 0).file,
                "aba",
                SourceFindOptions::new(),
                RequestOptions::new(),
            )
            .await;
        assert!(matches!(
            result,
            Err(ClientError::ProtocolFeatureUnavailable)
        ));
        server.await.unwrap();
    }
}
