//! Public compatibility checks for Rootlight MCP domain-error envelopes.
//!
//! These tests pin additive 1.x behavior and fail-closed handling of unknown majors.

use rootlight_mcp_contract::{ErrorCode, ErrorResponse, NextAction, SchemaVersion};
use serde_json::{Value, json};

#[test]
fn additive_error_details_preserve_code_and_actions() {
    let golden: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/errors/mcp-error-goldens-1.0.json"
    ))
    .expect("checked error goldens are valid JSON");
    let baseline: ErrorResponse = serde_json::from_value(golden["envelopes"][0].clone())
        .expect("baseline envelope satisfies the public contract");
    let additive: ErrorResponse = serde_json::from_str(include_str!(
        "../../../tests/fixtures/errors/mcp-error-envelope-1.0-additive-details.json"
    ))
    .expect("additive detail fixture satisfies the public contract");

    assert_eq!(baseline.error.code(), ErrorCode::InvalidCursor);
    assert_eq!(additive.error.code(), baseline.error.code());
    assert_eq!(additive.error.next_actions(), baseline.error.next_actions());
    assert_eq!(
        additive.error.next_actions(),
        [NextAction::RestartEnumeration]
    );
    assert_eq!(baseline.error.details().len(), 1);
    assert_eq!(additive.error.details().len(), 2);
}

#[test]
fn unsupported_error_envelope_major_is_rejected() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/errors/mcp-error-envelope-2.0-unsupported.json"
    ))
    .expect("unsupported-major fixture is valid JSON");

    assert!(serde_json::from_value::<ErrorResponse>(fixture.clone()).is_err());

    let mut supported = fixture;
    supported["schema_version"] = json!("1.0");
    let supported: ErrorResponse = serde_json::from_value(supported)
        .expect("the same envelope with the current major satisfies the contract");
    assert_eq!(supported.error.code(), ErrorCode::ProtocolMismatch);
    assert_eq!(
        supported.error.next_actions(),
        [NextAction::SelectSupportedVersion]
    );
}

#[test]
fn current_error_envelope_round_trips() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/errors/mcp-error-envelope-1.0-additive-details.json"
    ))
    .expect("current error fixture is valid JSON");
    let decoded: ErrorResponse = serde_json::from_value(fixture.clone())
        .expect("current error fixture satisfies the public contract");

    assert_eq!(decoded.schema_version, SchemaVersion::V1_0);
    let encoded = serde_json::to_value(&decoded).expect("current error envelope serializes");
    assert_eq!(encoded, fixture);
    let round_tripped: ErrorResponse =
        serde_json::from_value(encoded).expect("serialized error envelope decodes");
    assert_eq!(round_tripped, decoded);
}
