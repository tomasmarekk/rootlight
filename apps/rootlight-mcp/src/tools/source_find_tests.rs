//! Outer source.find wire guards complement the executor's authenticated paging.
//! Escaped JSON input size is independent of the decoded literal byte allowance.

use super::*;
use rootlight_ids::{ContentHash, FileId, GenerationId, RepositoryId};
use rootlight_ir::{SourceRef, SourceSpan};
use rootlight_mcp_contract::{
    source_find::{
        SourceFindData, SourceFindEnvelope, SourceFindFile, SourceFindSchemaVersion, SourceFindStop,
    },
    source_text::SourceTextEncoding,
    vertical::{CacheStatus, RequiredNullable, ResolvedRepository, UsageSummary},
};
use serde_json::json;

pub(super) fn input(literal: &str) -> Value {
    json!({
        "repository": {"repository_id": RepositoryId::from_bytes([1; 16])},
        "generation": GenerationId::from_bytes([2; 20]),
        "file_id": FileId::from_bytes([3; 20]),
        "literal": literal,
    })
}

#[test]
fn source_find_escaped_literal_admission_uses_decoded_and_wire_byte_limits() {
    let maximum = input(&"\0".repeat(65_536));
    let wire = serde_json::to_vec(&maximum).unwrap();
    assert!(wire.len() > 6 * 65_536);
    assert!(tool_argument_bytes_are_valid(
        VerticalTool::SourceFind,
        &maximum
    ));
    assert!(decode_typed_input(VerticalTool::SourceFind, &maximum).is_ok());
    for literal in ["\0".repeat(65_537), "😀".repeat(16_385)] {
        let oversized = input(&literal);
        assert!(tool_argument_bytes_are_valid(
            VerticalTool::SourceFind,
            &oversized
        ));
        assert!(decode_typed_input(VerticalTool::SourceFind, &oversized).is_err());
    }
    let wire_oversized = input(&"\0".repeat(100_000));
    assert!(!tool_argument_bytes_are_valid(
        VerticalTool::SourceFind,
        &wire_oversized
    ));
    assert!(!tool_argument_bytes_are_valid(
        VerticalTool::SourceRead,
        &maximum
    ));
}

fn output() -> Value {
    let repository = RepositoryId::from_bytes([1; 16]);
    let generation = GenerationId::from_bytes([2; 20]);
    let file = FileId::from_bytes([3; 20]);
    let hash = ContentHash::from_bytes([4; 32]);
    serde_json::to_value(SourceFindOutput::Success(SourceFindEnvelope {
        schema_version: SourceFindSchemaVersion::V1_1,
        repository: ResolvedRepository {
            repository_id: repository,
            display_name: "fixture".to_owned(),
        },
        generation,
        data: SourceFindData {
            file: SourceFindFile {
                id: file,
                path: "file.e".to_owned(),
                language: "eiffel".to_owned(),
                content_hash: hash,
                byte_length: 6,
                encoding: SourceTextEncoding::Utf8,
                generated: false,
                trust: TrustClassification::UntrustedRepositoryData,
                extracted_from: None,
            },
            matches: vec![SourceRef::new(
                repository,
                generation,
                SourceSpan::new(file, 1, 4).unwrap(),
                hash,
                None,
            )],
            scanned_start_byte: 0,
            scanned_end_byte: 6,
            restoration_bytes: 0,
            stop: SourceFindStop::EndOfSource,
            total_metadata_bytes: 12,
            total_response_memory_bytes: 300,
        },
        truncated: false,
        next_cursor: RequiredNullable(None),
        usage: UsageSummary {
            rows: 2,
            edges: 0,
            source_bytes: 0,
            json_bytes: 0,
            estimated_tokens: 0,
            wall_time_ms: 0,
            cache_status: CacheStatus::NotApplicable,
            trace_id: "source-find-test".to_owned(),
        },
        trust: TrustClassification::UntrustedRepositoryData,
    }))
    .unwrap()
}

#[test]
fn source_find_selects_only_its_current_duration_contract() {
    let contract = ToolContract::compile(VerticalTool::SourceFind).unwrap();
    for requested in [None, Some("1.1")] {
        assert_eq!(
            ContractSelection::select(&contract, requested),
            Some(ContractSelection::Current)
        );
    }
    assert_eq!(ContractSelection::select(&contract, Some("1.0")), None);
    let error = unsupported_contract_version_error(&contract).unwrap();
    assert_eq!(error.code(), ErrorCode::ProtocolMismatch);
    assert_eq!(
        error
            .details()
            .get(&DetailKey::parse("supported_version").unwrap()),
        Some(&PublicValue::Label(SafeLabel::parse("1.1").unwrap()))
    );

    let output = select_output_version(
        VerticalTool::SourceFind,
        ContractSelection::Current,
        output(),
    )
    .unwrap();
    let input = decode_typed_input(VerticalTool::SourceFind, &input("abc")).unwrap();
    assert_eq!(output["schema_version"], "1.1");
    assert!(contract.output_validator.is_valid(&output));
    assert!(typed_output_is_valid(
        VerticalTool::SourceFind,
        &input,
        &output
    ));

    let HandlerResponse::Success(rejected) =
        tool_error(&contract, ContractSelection::Current, error).unwrap()
    else {
        panic!("unsupported contract returns a checked tool error")
    };
    let error = &rejected["structuredContent"];
    assert_eq!(rejected["isError"], true);
    assert_eq!(error["schema_version"], "1.1");
    assert_eq!(error["error"]["code"], "PROTOCOL_MISMATCH");
    assert!(contract.output_validator.is_valid(error));
    assert!(typed_error_output_is_valid(VerticalTool::SourceFind, error));
}

#[test]
fn source_find_outer_output_validation_binds_request_and_rejects_forged_evidence() {
    let request = decode_typed_input(VerticalTool::SourceFind, &input("abc")).unwrap();
    let valid = output();
    assert!(typed_output_is_valid(
        VerticalTool::SourceFind,
        &request,
        &valid
    ));
    for (pointer, value) in [
        ("/data/matches/0/span/end_byte", json!(5)),
        (
            "/data/matches/0/content_hash",
            json!(ContentHash::from_bytes([9; 32])),
        ),
        ("/data/file/id", json!(FileId::from_bytes([9; 20]))),
        ("/generation", json!(GenerationId::from_bytes([9; 20]))),
        ("/data/total_metadata_bytes", json!(0)),
        ("/data/scanned_end_byte", json!(5)),
        ("/data/stop", json!("match_limit")),
        ("/trust", json!("trusted")),
    ] {
        let mut forged = valid.clone();
        *forged.pointer_mut(pointer).unwrap() = value;
        assert!(
            !typed_output_is_valid(VerticalTool::SourceFind, &request, &forged),
            "{pointer}"
        );
    }
    for (field, value) in [
        ("file_id", json!(FileId::from_bytes([9; 20]))),
        ("generation", json!(GenerationId::from_bytes([9; 20]))),
        ("literal", json!("different width")),
        ("maximum_scan_bytes", json!(1)),
    ] {
        let mut changed = input("abc");
        changed[field] = value;
        let changed = decode_typed_input(VerticalTool::SourceFind, &changed).unwrap();
        assert!(
            !typed_output_is_valid(VerticalTool::SourceFind, &changed, &valid),
            "{field}"
        );
    }
}
