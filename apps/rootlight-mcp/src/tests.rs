use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    sync::{Notify, watch},
    task::JoinHandle,
    time::timeout,
};

use super::*;

struct PendingWriter {
    polled: watch::Sender<bool>,
    dropped: watch::Sender<bool>,
}

impl AsyncWrite for PendingWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
        _buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.polled.send_replace(true);
        Poll::Pending
    }

    fn poll_flush(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Pending
    }

    fn poll_shutdown(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl Drop for PendingWriter {
    fn drop(&mut self) {
        self.dropped.send_replace(true);
    }
}

const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1.0"}}}"#;
const INITIALIZED: &str = r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#;
const PING_TWO: &str = r#"{"jsonrpc":"2.0","id":"ping-2","method":"ping"}"#;

fn response(
    session: &mut Session,
    message: &str,
    limits: StdioLimits,
) -> Result<Option<Value>, SessionError> {
    session
        .handle_frame(message.as_bytes(), limits)?
        .map(|encoded| serde_json::from_slice(&encoded).map_err(SessionError::Serialization))
        .transpose()
}

fn initialize(session: &mut Session) {
    response(session, INITIALIZE, StdioLimits::default()).expect("initialize is handled");
    response(session, INITIALIZED, StdioLimits::default()).expect("initialized is handled");
}

fn assert_explicit_null_id(response: &Value) {
    let object = response
        .as_object()
        .expect("JSON-RPC response is an object");
    assert_eq!(object.get("id"), Some(&Value::Null));
}

#[test]
fn official_initialize_fixture_reaches_operation_with_truthful_capabilities() {
    let mut session = Session::rootlight();
    let initialize = response(&mut session, INITIALIZE, StdioLimits::default())
        .expect("initialize is handled")
        .expect("initialize has a response");
    assert_eq!(initialize["jsonrpc"], "2.0");
    assert_eq!(initialize["id"], 1);
    assert_eq!(
        initialize["result"]["protocolVersion"],
        MCP_SPECIFICATION_DATE
    );
    assert_eq!(initialize["result"]["capabilities"], json!({}));
    assert!(!session.is_operating());

    assert!(
        response(&mut session, INITIALIZED, StdioLimits::default())
            .expect("initialized is handled")
            .is_none()
    );
    assert!(session.is_operating());
}

#[test]
fn version_negotiation_returns_the_supported_revision() {
    let mut session = Session::rootlight();
    let request = INITIALIZE.replace("2025-11-25", "2099-01-01");
    let response = response(&mut session, &request, StdioLimits::default())
        .expect("initialize is handled")
        .expect("initialize has a response");
    assert_eq!(
        response["result"]["protocolVersion"],
        MCP_SPECIFICATION_DATE
    );
}

#[test]
fn initialize_accepts_official_icon_theme_and_open_extension_capabilities() {
    let mut session = Session::rootlight();
    let request = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{"example.vendor/boolean":true,"roots":{"listChanged":true}},"clientInfo":{"name":"fixture","version":"1.0","icons":[{"src":"data:image/png;base64,AA==","theme":"dark"}]}}}"#;
    let accepted = response(&mut session, request, StdioLimits::default())
        .expect("official initialize shape is handled")
        .expect("initialize has a response");
    assert_eq!(
        accepted["result"]["protocolVersion"],
        MCP_SPECIFICATION_DATE
    );
}

#[test]
fn initialize_rejects_invalid_known_capability_shapes() {
    let mut session = Session::rootlight();
    let request = r#"{"jsonrpc":"2.0","id":"known-cap","method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{"roots":{"listChanged":"yes"}},"clientInfo":{"name":"fixture","version":"1.0"}}}"#;
    let rejected = response(&mut session, request, StdioLimits::default())
        .expect("invalid capability is handled")
        .expect("invalid capability has an error");
    assert_eq!(rejected["id"], "known-cap");
    assert_eq!(rejected["error"]["code"], INVALID_PARAMS);
}

#[test]
fn initialize_rejects_unknown_critical_and_duplicate_fields() {
    for (params, id_is_readable) in [
        (
            r#"{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1","secret":"value"}}"#,
            true,
        ),
        (
            r#"{"protocolVersion":"2025-11-25","protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}"#,
            false,
        ),
    ] {
        let mut session = Session::rootlight();
        let request =
            format!(r#"{{"jsonrpc":"2.0","id":"strict","method":"initialize","params":{params}}}"#);
        let error = response(&mut session, &request, StdioLimits::default())
            .expect("invalid initialize is handled")
            .expect("invalid initialize has an error");
        if id_is_readable {
            assert_eq!(error["id"], "strict");
        } else {
            assert_explicit_null_id(&error);
        }
        assert!(matches!(
            error["error"]["code"].as_i64(),
            Some(value) if value == i64::from(INVALID_PARAMS) || value == i64::from(INVALID_REQUEST)
        ));
    }
}

#[test]
fn initialize_must_precede_ping_and_ping_accepts_request_meta_afterward() {
    let mut session = Session::rootlight();
    let before = response(&mut session, PING_TWO, StdioLimits::default())
        .expect("pre-initialize ping is handled")
        .expect("pre-initialize ping has an error");
    assert_eq!(before["id"], "ping-2");
    assert_eq!(before["error"]["code"], SERVER_NOT_INITIALIZED);

    response(&mut session, INITIALIZE, StdioLimits::default()).expect("initialize is handled");
    let waiting = r#"{"jsonrpc":"2.0","id":"ping-3","method":"ping","params":{"_meta":{"progressToken":"progress","vendor/value":true}}}"#;
    let ping = response(&mut session, waiting, StdioLimits::default())
        .expect("ping with request metadata is handled")
        .expect("ping has a response");
    assert_eq!(ping["id"], "ping-3");
    assert_eq!(ping["result"], json!({}));
}

#[test]
fn duplicate_initialize_does_not_reset_the_session() {
    let mut session = Session::rootlight();
    response(&mut session, INITIALIZE, StdioLimits::default()).expect("initialize is handled");
    let duplicate = response(
        &mut session,
        &INITIALIZE.replace(r#""id":1"#, r#""id":2"#),
        StdioLimits::default(),
    )
    .expect("duplicate is handled")
    .expect("duplicate has an error");
    assert_eq!(duplicate["id"], 2);
    assert_eq!(duplicate["error"]["code"], INVALID_REQUEST);
    response(&mut session, INITIALIZED, StdioLimits::default()).expect("initialized is handled");
    assert!(session.is_operating());
}

#[test]
fn notification_only_methods_used_as_requests_receive_errors() {
    let mut session = Session::rootlight();
    for request in [
        r#"{"jsonrpc":"2.0","id":"initialized-request","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":"cancel-request","method":"notifications/cancelled","params":{"requestId":1}}"#,
    ] {
        let error = response(&mut session, request, StdioLimits::default())
            .expect("request is handled")
            .expect("request has an error");
        assert_eq!(error["error"]["code"], INVALID_REQUEST);
        assert!(error["id"].is_string());
    }
}

#[test]
fn readable_ids_survive_unknown_shape_and_string_length_errors() {
    let mut session = Session::rootlight();
    for request in [
        r#"{"jsonrpc":"2.0","id":"unknown-field","method":"ping","unexpected":true}"#.to_owned(),
        format!(
            r#"{{"jsonrpc":"2.0","id":"long-method","method":"{}"}}"#,
            "m".repeat(MAX_METHOD_BYTES + 1)
        ),
        format!(
            r#"{{"jsonrpc":"2.0","id":"long-name","method":"initialize","params":{{"protocolVersion":"2025-11-25","capabilities":{{}},"clientInfo":{{"name":"{}","version":"1"}}}}}}"#,
            "n".repeat(MAX_IMPLEMENTATION_NAME_BYTES + 1)
        ),
    ] {
        let error = response(&mut session, &request, StdioLimits::default())
            .expect("invalid request is handled")
            .expect("invalid request has an error");
        assert!(error["id"].is_string());
    }
}

#[test]
fn malformed_batch_and_null_identity_do_not_echo_input() {
    let mut session = Session::rootlight();
    for input in [
        r#"{"jsonrpc":"2.0","id":"private-token","method":"ping""#,
        r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#,
        r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#,
    ] {
        let encoded = session
            .handle_frame(input.as_bytes(), StdioLimits::default())
            .expect("invalid input is handled")
            .expect("invalid input has an error");
        assert!(!String::from_utf8_lossy(&encoded).contains("private-token"));
        let error: Value = serde_json::from_slice(&encoded).expect("error response is valid JSON");
        assert_explicit_null_id(&error);
        assert!(matches!(
            error["error"]["code"].as_i64(),
            Some(value) if value == i64::from(PARSE_ERROR) || value == i64::from(INVALID_REQUEST)
        ));
    }
}

#[test]
fn every_accepted_json_number_round_trips_as_a_request_identity() {
    let mut session = Session::rootlight();
    initialize(&mut session);

    for raw_id in ["1.5", "1e3", "18446744073709551616", "-9223372036854775809"] {
        let request = format!(r#"{{"jsonrpc":"2.0","id":{raw_id},"method":"ping"}}"#);
        let response = response(&mut session, &request, StdioLimits::default())
            .expect("numeric identity is handled")
            .expect("ping has a response");
        let expected: Value =
            serde_json::from_str(raw_id).expect("numeric identity fixture is valid JSON");
        assert_eq!(response["id"], expected);
    }
}

#[test]
fn numeric_identity_ordering_is_total_and_consistent_with_equality() {
    let ids = ["-0.0", "0.0", "1", "1.0", "1e3", "18446744073709551616"].map(|raw| {
        let value: Value =
            serde_json::from_str(raw).expect("numeric identity fixture is valid JSON");
        RequestId::from_value(&value).expect("fixture is a request identity")
    });

    for left in &ids {
        for right in &ids {
            assert_eq!(*left == *right, left.cmp(right) == Ordering::Equal);
        }
    }
    assert_ne!(ids[0], ids[1]);
    assert_ne!(ids[0].cmp(&ids[1]), Ordering::Equal);
    let mut sorted = ids;
    sorted.sort();
    assert!(sorted.windows(2).all(|pair| pair[0] <= pair[1]));
}

#[test]
fn arbitrary_precision_identity_texts_do_not_alias_through_f64() {
    let ids = [
        "1.0000000000000000000000000000001",
        "1.0000000000000000000000000000002",
    ]
    .map(|raw| {
        let value: Value =
            serde_json::from_str(raw).expect("numeric identity fixture is valid JSON");
        RequestId::from_value(&value).expect("fixture is a request identity")
    });
    let rendered = ids.each_ref().map(|id| match id {
        RequestId::Number(number) => number.to_string(),
        RequestId::String(_) => unreachable!("fixtures are numeric"),
    });

    // Default serde_json rounds both fixtures to one f64. When its
    // arbitrary-precision feature is unified, their original texts survive.
    if rendered[0] != rendered[1] {
        assert_ne!(ids[0], ids[1]);
        assert_ne!(ids[0].cmp(&ids[1]), Ordering::Equal);
    }
}

#[test]
fn duplicate_names_are_rejected_before_member_accounting_collapses_them() {
    let mut session = Session::rootlight();
    let duplicate = r#"{"jsonrpc":"2.0","id":"first","id":"second","method":"ping"}"#;
    let error = response(&mut session, duplicate, StdioLimits::default())
        .expect("duplicate input is handled")
        .expect("duplicate input has an error");
    assert_explicit_null_id(&error);
    assert_eq!(error["error"]["code"], INVALID_REQUEST);

    let nested =
        r#"{"jsonrpc":"2.0","id":"nested","method":"ping","params":{"_meta":{"same":1,"same":2}}}"#;
    let error = response(&mut session, nested, StdioLimits::default())
        .expect("nested duplicate input is handled")
        .expect("nested duplicate input has an error");
    assert_explicit_null_id(&error);
    assert_eq!(error["error"]["code"], INVALID_REQUEST);
}

#[test]
fn json_depth_string_object_array_and_node_limits_are_inclusive() {
    let mut session = Session::rootlight();
    initialize(&mut session);

    let nested = r#"{"jsonrpc":"2.0","id":"depth","method":"ping","params":{"_meta":{"x":{}}}}"#;
    let accepted = response(
        &mut session,
        nested,
        StdioLimits::default().with_max_json_depth(3),
    )
    .expect("exact depth is handled")
    .expect("exact depth has a response");
    assert_eq!(accepted["result"], json!({}));
    let rejected = response(
        &mut session,
        nested,
        StdioLimits::default().with_max_json_depth(2),
    )
    .expect("excess depth is handled")
    .expect("excess depth has an error");
    assert_eq!(rejected["error"]["code"], INVALID_REQUEST);

    let exact_string =
        r#"{"jsonrpc":"2.0","id":"string","method":"ping","params":{"_meta":{"x":"12345678"}}}"#;
    assert!(
        response(
            &mut session,
            exact_string,
            StdioLimits::default().with_max_string_bytes(8),
        )
        .expect("exact string is handled")
        .is_some()
    );
    let long_string = exact_string.replace("12345678", "123456789");
    let rejected = response(
        &mut session,
        &long_string,
        StdioLimits::default().with_max_string_bytes(8),
    )
    .expect("long string is handled")
    .expect("long string has an error");
    assert_explicit_null_id(&rejected);
    assert_eq!(rejected["error"]["code"], INVALID_REQUEST);

    let array = r#"{"jsonrpc":"2.0","id":"array","method":"ping","params":{"_meta":{"x":[1,2]}}}"#;
    assert!(
        response(
            &mut session,
            array,
            StdioLimits::default().with_max_array_items(2),
        )
        .expect("exact array is handled")
        .is_some()
    );
    let exceeded = array.replace("[1,2]", "[1,2,3]");
    let rejected = response(
        &mut session,
        &exceeded,
        StdioLimits::default().with_max_array_items(2),
    )
    .expect("long array is handled")
    .expect("long array has an error");
    assert_eq!(rejected["error"]["code"], INVALID_REQUEST);

    let simple = r#"{"jsonrpc":"2.0","id":"nodes","method":"ping"}"#;
    assert!(
        response(
            &mut session,
            simple,
            StdioLimits::default()
                .with_max_json_nodes(4)
                .with_max_object_properties(3),
        )
        .expect("exact node and property limits are handled")
        .is_some()
    );
    for limits in [
        StdioLimits::default().with_max_json_nodes(3),
        StdioLimits::default().with_max_object_properties(2),
    ] {
        let rejected = response(&mut session, simple, limits)
            .expect("limit violation is handled")
            .expect("limit violation has an error");
        assert_eq!(rejected["error"]["code"], INVALID_REQUEST);
    }
}

#[test]
fn configured_collection_limits_win_before_hostile_near_frame_tails_are_parsed() {
    let base_limits = JsonLimits {
        max_depth: DEFAULT_MAX_JSON_DEPTH,
        max_string_bytes: DEFAULT_MAX_STRING_BYTES,
        max_object_properties: DEFAULT_MAX_OBJECT_PROPERTIES,
        max_array_items: DEFAULT_MAX_ARRAY_ITEMS,
        max_nodes: DEFAULT_MAX_JSON_NODES,
    };
    let fixtures = [
        (
            "[0,\"",
            JsonLimits {
                max_array_items: 1,
                ..base_limits
            },
        ),
        (
            "{\"safe\":0,\"",
            JsonLimits {
                max_object_properties: 1,
                ..base_limits
            },
        ),
    ];

    for (prefix, limits) in fixtures {
        let mut hostile = String::with_capacity(DEFAULT_MAX_FRAME_BYTES);
        hostile.push_str(prefix);
        hostile.push_str(&"x".repeat(DEFAULT_MAX_FRAME_BYTES - hostile.len()));
        let failure =
            parse_bounded(hostile.as_bytes(), limits).expect_err("collection limit is enforced");
        assert_eq!(failure, ParseFailure::Rejected(JsonIssue::Limits));
    }
}

#[test]
fn escaped_string_limits_abort_before_hostile_keys_or_values_are_materialized() {
    let limits = JsonLimits {
        max_depth: DEFAULT_MAX_JSON_DEPTH,
        max_string_bytes: 8,
        max_object_properties: DEFAULT_MAX_OBJECT_PROPERTIES,
        max_array_items: DEFAULT_MAX_ARRAY_ITEMS,
        max_nodes: DEFAULT_MAX_JSON_NODES,
    };

    for prefix in [b"{\"".as_slice(), b"{\"safe\":\"".as_slice()] {
        let mut hostile = Vec::with_capacity(900 * 1024);
        hostile.extend_from_slice(prefix);
        for _ in 0..9 {
            hostile.extend_from_slice(br"\u0061");
        }
        hostile.resize(900 * 1024, b'x');

        let failure =
            parse_bounded(&hostile, limits).expect_err("decoded string limit is enforced");
        assert_eq!(failure, ParseFailure::Rejected(JsonIssue::Limits));
    }
}

#[test]
fn string_preflight_counts_utf8_and_surrogate_escapes_by_decoded_bytes() {
    let fixture = r#"{"é":"\uD83D\uDE00"}"#;
    let limits = JsonLimits {
        max_depth: 1,
        max_string_bytes: 4,
        max_object_properties: 1,
        max_array_items: 1,
        max_nodes: 2,
    };
    assert!(parse_bounded(fixture.as_bytes(), limits).is_ok());
    let failure = parse_bounded(
        fixture.as_bytes(),
        JsonLimits {
            max_string_bytes: 3,
            ..limits
        },
    )
    .expect_err("four-byte scalar exceeds a three-byte limit");
    assert_eq!(failure, ParseFailure::Rejected(JsonIssue::Limits));
}

#[test]
fn hostile_fragmented_array_uses_logarithmic_reservation_growth() {
    let item_count = 16 * 1024;
    let mut input = Vec::with_capacity(item_count * 2 + 1);
    input.push(b'[');
    for index in 0..item_count {
        if index != 0 {
            input.push(b',');
        }
        input.push(b'0');
    }
    input.push(b']');
    let limits = JsonLimits {
        max_depth: 1,
        max_string_bytes: 1,
        max_object_properties: 1,
        max_array_items: item_count,
        max_nodes: item_count + 1,
    };

    let (value, growths) = json::parse_bounded_with_array_growths(&input, limits)
        .expect("exact-limit array is accepted");
    assert_eq!(
        value.as_array().map(Vec::len),
        Some(item_count),
        "all array items are retained"
    );
    let logarithmic_bound = usize::try_from(usize::BITS).expect("usize bit width fits in usize");
    assert!(growths <= logarithmic_bound);
}

#[test]
fn response_limit_is_inclusive_and_bounds_serialization() {
    let id = RequestId::String("response-boundary".to_owned());
    let encoded = result_response(&id, &EmptyObject {}, StdioLimits::default())
        .expect("default response is encoded");
    let exact = result_response(
        &id,
        &EmptyObject {},
        StdioLimits::default().with_max_response_bytes(encoded.len()),
    )
    .expect("exact response limit is inclusive");
    assert_eq!(exact, encoded);
    let error = result_response(
        &id,
        &EmptyObject {},
        StdioLimits::default().with_max_response_bytes(encoded.len() - 1),
    )
    .expect_err("one byte below the response length is rejected");
    assert!(matches!(error, SessionError::ResponseTooLarge));
}

#[test]
fn hostile_escaped_identity_uses_logarithmic_response_reservations() {
    let id = RequestId::String("\0".repeat(32 * 1024));
    let response = ResultResponse {
        jsonrpc: JSON_RPC_VERSION,
        id: &id,
        result: &EmptyObject {},
    };
    let mut writer = BoundedResponseWriter::new(DEFAULT_MAX_RESPONSE_BYTES);
    serde_json::to_writer(&mut writer, &response).expect("bounded response is encoded");

    let logarithmic_bound = usize::try_from(usize::BITS).expect("usize bit width fits in usize");
    assert!(writer.reservation_growths <= logarithmic_bound);
    assert!(writer.bytes.len() <= DEFAULT_MAX_RESPONSE_BYTES);
}

#[tokio::test]
async fn frame_limit_is_inclusive_and_oversized_input_is_terminal() {
    let exact = format!("{PING_TWO}\n").into_bytes();
    let mut frames = FrameReader::new(BufReader::new(exact.as_slice()), PING_TWO.len());
    assert!(matches!(
        frames.read_next().await.expect("exact frame is read"),
        ReadFrame::Complete(frame) if frame == PING_TWO.as_bytes()
    ));

    let oversized = vec![b'x'; PING_TWO.len() + 1];
    let mut frames = FrameReader::new(BufReader::new(oversized.as_slice()), PING_TWO.len());
    assert!(matches!(
        frames
            .read_next()
            .await
            .expect("oversized frame is reported"),
        ReadFrame::Oversized
    ));
}

#[tokio::test]
async fn raw_embedded_newline_is_not_accepted_as_one_frame() {
    let bytes = b"{\"jsonrpc\":\"2.0\",\n\"id\":1,\"method\":\"initialize\"}\n";
    let mut frames = FrameReader::new(BufReader::new(bytes.as_slice()), DEFAULT_MAX_FRAME_BYTES);
    assert!(matches!(
        frames
            .read_next()
            .await
            .expect("first raw line is read"),
        ReadFrame::Complete(frame) if frame == b"{\"jsonrpc\":\"2.0\","
    ));
    assert!(matches!(
        frames
            .read_next()
            .await
            .expect("second raw line is read"),
        ReadFrame::Complete(frame) if frame == b"\"id\":1,\"method\":\"initialize\"}"
    ));
}

#[tokio::test]
async fn partial_frame_survives_a_cancelled_read_future() {
    let (client, server) = tokio::io::duplex(1024);
    let (_client_read, mut client_write) = tokio::io::split(client);
    let (server_read, _server_write) = tokio::io::split(server);
    let mut frames = FrameReader::new(BufReader::new(server_read), DEFAULT_MAX_FRAME_BYTES);
    let split = PING_TWO.len() / 2;

    client_write
        .write_all(&PING_TWO.as_bytes()[..split])
        .await
        .expect("partial fixture writes");
    assert!(
        timeout(Duration::from_millis(20), frames.read_next())
            .await
            .is_err(),
        "partial frame remains pending"
    );

    client_write
        .write_all(&PING_TWO.as_bytes()[split..])
        .await
        .expect("remaining fixture writes");
    client_write
        .write_all(b"\n")
        .await
        .expect("fixture newline writes");
    assert!(matches!(
        frames.read_next().await.expect("complete frame is retained"),
        ReadFrame::Complete(frame) if frame == PING_TWO.as_bytes()
    ));
}

#[tokio::test]
async fn unterminated_oversized_source_is_rejected_after_maximum_plus_one_bytes() {
    let maximum = 32;
    let source = tokio::io::repeat(b'x');
    let mut frames = FrameReader::new(BufReader::with_capacity(1, source), maximum);
    assert!(matches!(
        timeout(Duration::from_secs(1), frames.read_next())
            .await
            .expect("bounded work completes")
            .expect("infinite source remains readable"),
        ReadFrame::Oversized
    ));
}

#[tokio::test]
async fn one_byte_frame_fragments_use_logarithmic_reservation_growth() {
    let maximum = 16 * 1024;
    let mut input = vec![b'x'; maximum];
    input.push(b'\n');
    let mut frames = FrameReader::new(BufReader::with_capacity(1, input.as_slice()), maximum);
    assert!(matches!(
        frames.read_next().await.expect("fragmented frame is read"),
        ReadFrame::Complete(frame) if frame.len() == maximum
    ));
    let logarithmic_bound = usize::try_from(usize::BITS).expect("usize bit width fits in usize");
    assert!(frames.reservation_growths <= logarithmic_bound);
}

#[tokio::test]
async fn serve_closes_after_an_oversized_unterminated_frame() {
    let maximum = 32;
    let mut input = vec![b'x'; maximum + 1];
    input.extend_from_slice(format!("\n{PING_TWO}\n").as_bytes());
    let (server_output, mut client_output) = tokio::io::duplex(4096);
    let server = tokio::spawn(async move {
        let mut session = Session::rootlight();
        serve(
            BufReader::new(io::Cursor::new(input)),
            server_output,
            &mut session,
            Arc::new(NoopRequestHandler),
            StdioLimits::default().with_max_frame_bytes(maximum),
        )
        .await
    });

    server
        .await
        .expect("server task joins")
        .expect("oversized input closes the session cleanly");
    let mut output = Vec::new();
    client_output
        .read_to_end(&mut output)
        .await
        .expect("response output reads");
    let responses: Vec<Value> = output
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).expect("response line is valid JSON"))
        .collect();
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0]["error"]["code"], INVALID_REQUEST);
}

#[tokio::test]
async fn dropping_serve_aborts_and_drops_a_pending_response_writer() {
    let (mut client_input, server_input) = tokio::io::duplex(2048);
    let (polled_tx, mut polled_rx) = watch::channel(false);
    let (dropped_tx, mut dropped_rx) = watch::channel(false);
    let server = tokio::spawn(async move {
        let mut session = Session::rootlight();
        serve(
            BufReader::new(server_input),
            PendingWriter {
                polled: polled_tx,
                dropped: dropped_tx,
            },
            &mut session,
            Arc::new(NoopRequestHandler),
            StdioLimits::default(),
        )
        .await
    });

    client_input
        .write_all(INITIALIZE.as_bytes())
        .await
        .expect("initialize writes");
    client_input
        .write_all(b"\n")
        .await
        .expect("initialize delimiter writes");
    client_input.flush().await.expect("initialize flushes");
    timeout(Duration::from_secs(1), async {
        while !*polled_rx.borrow() {
            polled_rx
                .changed()
                .await
                .expect("pending writer keeps its signal open");
        }
    })
    .await
    .expect("response writer is polled");

    server.abort();
    let join_error = server.await.expect_err("serve task is cancelled");
    assert!(join_error.is_cancelled());
    timeout(Duration::from_secs(1), async {
        while !*dropped_rx.borrow() {
            dropped_rx
                .changed()
                .await
                .expect("pending writer drops after cancellation");
        }
    })
    .await
    .expect("cancelled serve drops its response writer");
}

#[derive(Clone)]
struct WaitingHandler {
    started: Arc<Notify>,
    cancelled: Arc<Notify>,
}

impl RequestHandler for WaitingHandler {
    fn handle(
        &self,
        request: OperatingRequest,
        mut cancellation: RequestCancellation,
    ) -> HandlerFuture {
        let started = Arc::clone(&self.started);
        let cancelled = Arc::clone(&self.cancelled);
        Box::pin(async move {
            if request.method() != "test/wait" {
                return HandlerResponse::error(METHOD_NOT_FOUND, "method is not available");
            }
            started.notify_one();
            cancellation.cancelled().await;
            cancelled.notify_one();
            HandlerResponse::Cancelled
        })
    }
}

async fn write_message(writer: &mut tokio::io::WriteHalf<tokio::io::DuplexStream>, message: &str) {
    writer
        .write_all(message.as_bytes())
        .await
        .expect("fixture message writes");
    writer
        .write_all(b"\n")
        .await
        .expect("fixture newline writes");
}

async fn read_message(
    reader: &mut BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
) -> Value {
    let mut line = String::new();
    timeout(Duration::from_secs(2), reader.read_line(&mut line))
        .await
        .expect("response arrives before timeout")
        .expect("response reads");
    serde_json::from_str(&line).expect("response is valid JSON")
}

async fn start_waiting_server(
    limits: StdioLimits,
) -> (
    tokio::io::WriteHalf<tokio::io::DuplexStream>,
    BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
    Arc<Notify>,
    Arc<Notify>,
    JoinHandle<Result<(), SessionError>>,
) {
    let (client, server) = tokio::io::duplex(32 * 1024);
    let (server_read, server_write) = tokio::io::split(server);
    let (client_read, client_write) = tokio::io::split(client);
    let started = Arc::new(Notify::new());
    let cancelled = Arc::new(Notify::new());
    let handler = WaitingHandler {
        started: Arc::clone(&started),
        cancelled: Arc::clone(&cancelled),
    };
    let task = tokio::spawn(async move {
        let mut session = Session::rootlight();
        serve(
            BufReader::new(server_read),
            server_write,
            &mut session,
            Arc::new(handler),
            limits,
        )
        .await
    });
    (
        client_write,
        BufReader::new(client_read),
        started,
        cancelled,
        task,
    )
}

async fn initialize_duplex(
    writer: &mut tokio::io::WriteHalf<tokio::io::DuplexStream>,
    reader: &mut BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
) {
    write_message(writer, INITIALIZE).await;
    let initialized = read_message(reader).await;
    assert_eq!(initialized["id"], 1);
    write_message(writer, INITIALIZED).await;
}

#[tokio::test]
async fn cancellation_is_read_while_a_request_is_in_flight() {
    let (mut writer, mut reader, started, cancelled, task) =
        start_waiting_server(StdioLimits::default()).await;
    initialize_duplex(&mut writer, &mut reader).await;

    write_message(
        &mut writer,
        r#"{"jsonrpc":"2.0","id":"wait-2","method":"test/wait","params":{}}"#,
    )
    .await;
    timeout(Duration::from_secs(2), started.notified())
        .await
        .expect("handler starts");

    write_message(
        &mut writer,
        r#"{"jsonrpc":"2.0","id":"ping-3","method":"ping","params":{"_meta":{"probe":true}}}"#,
    )
    .await;
    let ping = read_message(&mut reader).await;
    assert_eq!(ping["id"], "ping-3");

    write_message(
        &mut writer,
        r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"wait-2","reason":"fixture"}}"#,
    )
    .await;
    timeout(Duration::from_secs(2), cancelled.notified())
        .await
        .expect("in-flight handler observes cancellation");
    writer.shutdown().await.expect("fixture input closes");
    task.await
        .expect("server task joins")
        .expect("server exits cleanly");
}

#[tokio::test]
async fn duplicate_in_flight_ids_are_rejected_without_cancelling_the_original() {
    let limits = StdioLimits::default().with_max_in_flight_requests(1);
    let (mut writer, mut reader, started, cancelled, task) = start_waiting_server(limits).await;
    initialize_duplex(&mut writer, &mut reader).await;

    let request = r#"{"jsonrpc":"2.0","id":"duplicate","method":"test/wait","params":{}}"#;
    write_message(&mut writer, request).await;
    timeout(Duration::from_secs(2), started.notified())
        .await
        .expect("first handler starts");
    write_message(&mut writer, request).await;
    let duplicate = read_message(&mut reader).await;
    assert_eq!(duplicate["id"], "duplicate");
    assert_eq!(duplicate["error"]["code"], INVALID_REQUEST);

    write_message(
        &mut writer,
        r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"duplicate"}}"#,
    )
    .await;
    timeout(Duration::from_secs(2), cancelled.notified())
        .await
        .expect("original handler receives cancellation");
    writer.shutdown().await.expect("fixture input closes");
    task.await
        .expect("server task joins")
        .expect("server exits cleanly");
}

#[tokio::test]
async fn malformed_unknown_and_late_cancellations_do_not_consume_strikes() {
    let limits = StdioLimits::default().with_max_invalid_messages(2);
    let (mut writer, mut reader, _started, _cancelled, task) = start_waiting_server(limits).await;
    initialize_duplex(&mut writer, &mut reader).await;

    for message in [
        r#"{"jsonrpc":"2.0","method":"notifications/cancelled"}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":17}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"unknown"}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1,"unexpected":true}}"#,
    ] {
        write_message(&mut writer, message).await;
    }
    write_message(&mut writer, PING_TWO).await;
    let ping = read_message(&mut reader).await;
    assert_eq!(ping["id"], "ping-2");

    writer.shutdown().await.expect("fixture input closes");
    task.await
        .expect("server task joins")
        .expect("server exits cleanly");
}

#[test]
fn session_and_error_debug_output_redacts_peer_and_source_details() {
    let id = RequestId::String("private-token".to_owned());
    assert!(!format!("{id:?}").contains("private-token"));
    let error = SessionError::Io(io::Error::other("private-source-path"));
    let debug = format!("{error:?}");
    assert!(!debug.contains("private-source-path"));
    assert_eq!(debug, r#"SessionError { category: "io" }"#);
}

#[test]
fn invalid_local_limits_fail_before_peer_processing() {
    let mut session = Session::rootlight();
    let excessive_tokio_capacity = Semaphore::MAX_PERMITS
        .checked_add(1)
        .expect("Tokio's permit ceiling leaves room for an invalid value");
    for limits in [
        StdioLimits::default().with_max_response_bytes(0),
        StdioLimits::default().with_max_json_depth(MAX_SUPPORTED_JSON_DEPTH + 1),
        StdioLimits::default().with_max_in_flight_requests(0),
        StdioLimits::default().with_response_channel_capacity(0),
        StdioLimits::default().with_response_channel_capacity(excessive_tokio_capacity),
        StdioLimits::default().with_max_blocking_workers(0),
        StdioLimits::default().with_max_blocking_workers(excessive_tokio_capacity),
    ] {
        let error = session
            .handle_frame(PING_TWO.as_bytes(), limits)
            .expect_err("invalid limits are rejected");
        assert!(matches!(error, SessionError::InvalidLimits));
    }

    let error = StdioLimits::default()
        .with_max_blocking_workers(excessive_tokio_capacity)
        .blocking_pool()
        .expect_err("oversized blocking pool is rejected before semaphore construction");
    assert!(matches!(error, SessionError::InvalidLimits));
}
