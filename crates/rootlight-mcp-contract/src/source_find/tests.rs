//! Behavioral guards for exact source identity and authenticated scan continuation.
//! These exercise wire consumers and false completeness claims independently of
//! the source matcher's scalar-search tests.

use rootlight_ir::SourceSpan;
use serde_json::{Value, json};

use super::*;
use crate::vertical::{CacheStatus, RepositoryIdSelector};

fn repository() -> RepositoryId {
    RepositoryId::from_bytes([1; 16])
}

fn generation() -> GenerationId {
    GenerationId::from_bytes([2; 20])
}

fn file() -> SourceFindFile {
    SourceFindFile {
        id: FileId::from_bytes([3; 20]),
        path: "source/example.e".to_owned(),
        language: "eiffel".to_owned(),
        content_hash: ContentHash::from_bytes([4; 32]),
        byte_length: 100,
        encoding: SourceTextEncoding::Utf8,
        generated: false,
        trust: TrustClassification::UntrustedRepositoryData,
        extracted_from: None,
    }
}

fn source(file: &SourceFindFile, start: u64, end: u64) -> SourceRef {
    SourceRef::new(
        repository(),
        generation(),
        SourceSpan::new(file.id, start, end).unwrap(),
        file.content_hash,
        None,
    )
}

fn input() -> SourceFindInput {
    SourceFindInput {
        repository: RepositorySelector::ById(RepositoryIdSelector {
            repository_id: repository(),
        }),
        generation: None,
        file_id: file().id,
        literal: "ababa".to_owned(),
        maximum_scan_bytes: None,
        maximum_matches: None,
        cursor: None,
        budget: None,
        response_profile: None,
    }
}

fn budgets() -> SourceFindCursorBudgets {
    SourceFindCursorBudgets {
        maximum_scan_bytes: MAX_SOURCE_FIND_SCAN_BYTES,
        maximum_matches: 128,
        maximum_query_bytes: 65_536,
        maximum_snapshot_bytes: 67_108_864,
        maximum_verified_bytes: u64::MAX,
        maximum_matcher_memory_bytes: 2_097_152,
        maximum_restore_bytes: 262_144,
        maximum_response_memory_bytes: 4_194_304,
        maximum_metadata_bytes: 65_536,
        maximum_results: 128,
        maximum_json_bytes: 524_288,
        maximum_tokens: 3200,
        timeout_ms: 2000,
        source_verification_duration_explicit: true,
    }
}

fn binding<'a>(
    file: &'a SourceFindFile,
    budgets: &'a SourceFindCursorBudgets,
) -> SourceFindCursorBinding<'a> {
    SourceFindCursorBinding {
        repository: repository(),
        generation: generation(),
        file,
        literal: "ababa",
        budgets,
        response_profile: ResponseProfile::Compact,
        exposure_profile: ExposureProfile::Scout,
        snapshot_id: [5; 32],
        key_id: 6,
    }
}

fn envelope() -> SourceFindEnvelope {
    let file = file();
    let total_metadata_bytes = u64::try_from(file.path.len() + file.language.len()).unwrap();
    SourceFindEnvelope {
        schema_version: SourceFindSchemaVersion::V1_1,
        repository: ResolvedRepository {
            repository_id: repository(),
            display_name: "sample".to_owned(),
        },
        generation: generation(),
        data: SourceFindData {
            matches: vec![source(&file, 0, 5), source(&file, 2, 7)],
            file,
            scanned_start_byte: 0,
            scanned_end_byte: 10,
            restoration_bytes: 0,
            stop: SourceFindStop::ScanLimit,
            total_metadata_bytes,
            total_response_memory_bytes: 300,
        },
        truncated: true,
        next_cursor: RequiredNullable(Some(
            ContinuationCursor::parse("authenticated-position").unwrap(),
        )),
        usage: UsageSummary {
            rows: 0,
            edges: 0,
            source_bytes: 0,
            json_bytes: 1000,
            estimated_tokens: 250,
            wall_time_ms: 2,
            cache_status: CacheStatus::NotApplicable,
            trace_id: "source-find".to_owned(),
        },
        trust: TrustClassification::UntrustedRepositoryData,
    }
}

#[test]
fn input_preserves_literal_and_rejects_unbounded_or_unserved_controls() {
    let mut value = input();
    value.literal = "Aa\r\nλ😀e\u{301}".to_owned();
    let wire = serde_json::to_value(&value).unwrap();
    assert_eq!(
        serde_json::from_value::<SourceFindInput>(wire.clone()).unwrap(),
        value
    );
    assert_eq!(value.effective_maximum_scan_bytes(), 67_108_864);
    assert_eq!(value.effective_maximum_matches(), 128);
    let schema = serde_json::to_value(schemars::schema_for!(SourceFindInput)).unwrap();
    let validator = jsonschema::draft202012::new(&schema).unwrap();
    assert!(validator.is_valid(&wire));
    let mut compact = wire.clone();
    compact["response_profile"] = json!("compact");
    assert!(validator.is_valid(&compact));
    assert_eq!(
        serde_json::from_value::<SourceFindInput>(compact)
            .unwrap()
            .response_profile,
        Some(ResponseProfile::Compact)
    );
    for (key, bad) in [
        ("literal", json!("")),
        ("literal", json!("a".repeat(65_537))),
        ("maximum_scan_bytes", json!(0)),
        ("maximum_scan_bytes", json!(67_108_865_u64)),
        ("maximum_matches", json!(0)),
        ("maximum_matches", json!(1025)),
        ("path", json!("arbitrary.e")),
        ("regex", json!(true)),
        ("case_sensitive", json!(false)),
        ("budget", json!({"max_source_bytes": 100})),
        ("response_profile", json!("standard")),
        ("response_profile", json!("evidence")),
    ] {
        let mut invalid = wire.clone();
        invalid[key] = bad;
        assert!(!validator.is_valid(&invalid), "schema admitted {key}");
        assert!(
            serde_json::from_value::<SourceFindInput>(invalid).is_err(),
            "decoder admitted {key}"
        );
    }
    let mut multibyte = wire;
    multibyte["literal"] = json!("😀".repeat(16_385));
    assert!(
        serde_json::from_value::<SourceFindInput>(multibyte).is_err(),
        "byte limit is independent of scalar count"
    );
    value.maximum_matches = Some(1024);
    assert!(value.validate().is_ok());
    assert!(!format!("{value:?}").contains(&value.literal));
    for profile in [ResponseProfile::Standard, ResponseProfile::Evidence] {
        value.response_profile = Some(profile);
        assert_eq!(value.validate(), Err(SourceFindContractError::InvalidInput));
    }
}

#[test]
fn continuation_requires_an_explicit_generation_and_opaque_bounded_wire() {
    let mut value = input();
    value.cursor = Some(ContinuationCursor::parse("opaque").unwrap());
    assert!(value.validate().is_err());
    value.generation = Some(GenerationSelector::Active(
        crate::vertical::ActiveGeneration::Active,
    ));
    assert!(value.validate().is_err());
    value.generation = Some(GenerationSelector::Explicit(generation()));
    assert!(value.validate().is_ok());
    let mut wire = serde_json::to_value(value).unwrap();
    wire["cursor"] = json!("x".repeat(4097));
    assert!(serde_json::from_value::<SourceFindInput>(wire).is_err());
}

#[test]
fn response_round_trips_overlaps_without_bodies_or_invented_totals() {
    let value = envelope();
    value.validate().unwrap();
    let wire = serde_json::to_value(SourceFindOutput::Success(value.clone())).unwrap();
    assert_eq!(
        serde_json::from_value::<SourceFindOutput>(wire.clone()).unwrap(),
        SourceFindOutput::Success(value)
    );
    let schema = serde_json::to_value(schemars::schema_for!(SourceFindOutput)).unwrap();
    let validator = jsonschema::draft202012::new(&schema).unwrap();
    assert!(validator.is_valid(&wire));
    for field in ["content", "matched_candidates", "total_matches", "coverage"] {
        let mut forged = wire.clone();
        forged["data"][field] = json!(1);
        assert!(!validator.is_valid(&forged));
        assert!(serde_json::from_value::<SourceFindOutput>(forged).is_err());
    }
    for stop in [
        SourceFindStop::MatchLimit,
        SourceFindStop::MatcherMemoryLimit,
        SourceFindStop::ResponseMemoryLimit,
        SourceFindStop::ResultLimit,
    ] {
        let mut limited = envelope();
        limited.data.stop = stop;
        limited.validate().unwrap();
    }
    let mut eof = envelope();
    eof.data.stop = SourceFindStop::EndOfSource;
    eof.data.scanned_end_byte = 100;
    eof.truncated = false;
    eof.next_cursor = RequiredNullable(None);
    eof.validate().unwrap();
    eof.data.matches.clear();
    eof.data.scanned_start_byte = 100;
    eof.validate().unwrap();
}

#[test]
fn decoder_rejects_foreign_duplicate_reordered_and_false_complete_evidence() {
    let wire = serde_json::to_value(envelope()).unwrap();
    let mut forgeries = Vec::<Value>::new();
    for (pointer, value) in [
        ("/schema_version", json!("1.0")),
        ("/trust", json!("trusted")),
        ("/data/file/trust", json!("trusted")),
        (
            "/data/matches/0/repository",
            json!(RepositoryId::from_bytes([9; 16])),
        ),
        (
            "/data/matches/0/generation",
            json!(GenerationId::from_bytes([9; 20])),
        ),
        (
            "/data/matches/0/span/file",
            json!(FileId::from_bytes([9; 20])),
        ),
        (
            "/data/matches/0/content_hash",
            json!(ContentHash::from_bytes([9; 32])),
        ),
        ("/data/scanned_end_byte", json!(101)),
        ("/data/scanned_start_byte", json!(10)),
        ("/data/restoration_bytes", json!(1)),
        ("/data/total_metadata_bytes", json!(301)),
        ("/data/stop", json!("end_of_source")),
        ("/truncated", json!(false)),
        ("/next_cursor", Value::Null),
        ("/usage/source_bytes", json!(1)),
    ] {
        let mut forged = wire.clone();
        *forged.pointer_mut(pointer).expect("known wire field") = value;
        forgeries.push(forged);
    }
    let mut duplicate = wire.clone();
    duplicate["data"]["matches"][1] = duplicate["data"]["matches"][0].clone();
    forgeries.push(duplicate);
    let mut reversed = wire;
    reversed["data"]["matches"]
        .as_array_mut()
        .unwrap()
        .reverse();
    forgeries.push(reversed);
    for stop in [
        "match_limit",
        "matcher_memory_limit",
        "response_memory_limit",
        "result_limit",
    ] {
        let mut empty = serde_json::to_value(envelope()).unwrap();
        empty["data"]["matches"] = json!([]);
        empty["data"]["stop"] = json!(stop);
        forgeries.push(empty);
    }
    for forged in forgeries {
        assert!(
            serde_json::from_value::<SourceFindEnvelope>(forged.clone()).is_err(),
            "admitted {forged}"
        );
    }
}

#[test]
fn encoding_and_extracted_origin_cannot_claim_foreign_or_unaligned_ranges() {
    for encoding in [SourceTextEncoding::Utf16Le, SourceTextEncoding::Utf16Be] {
        let mut value = envelope();
        value.data.file.encoding = encoding;
        value.data.scanned_start_byte = 6;
        value.data.scanned_end_byte = 20;
        value.data.restoration_bytes = 4;
        value.data.matches = vec![
            source(&value.data.file, 2, 12),
            source(&value.data.file, 6, 16),
        ];
        value.validate().unwrap();
        let origin = SourceRef::new(
            repository(),
            generation(),
            SourceSpan::new(FileId::from_bytes([8; 20]), 0, 120).unwrap(),
            ContentHash::from_bytes([7; 32]),
            None,
        );
        value.data.file.extracted_from = Some(origin.clone());
        value.validate().unwrap();
        value.data.file.extracted_from = Some(SourceRef::new(
            origin.repository(),
            origin.generation(),
            origin.span(),
            origin.content_hash(),
            Some(rootlight_ir::LineRange::new(1, 4).unwrap()),
        ));
        value.validate().unwrap();
        assert_eq!(
            serde_json::from_value::<SourceFindEnvelope>(serde_json::to_value(&value).unwrap())
                .unwrap(),
            value
        );
        let wire = serde_json::to_value(&value).unwrap();
        for (pointer, bad) in [
            ("/data/scanned_start_byte", json!(0)),
            ("/data/scanned_end_byte", json!(19)),
            ("/data/restoration_bytes", json!(5)),
            ("/data/restoration_bytes", json!(6)),
            ("/data/matches/0/span/start_byte", json!(3)),
            (
                "/data/file/extracted_from/repository",
                json!(RepositoryId::from_bytes([8; 16])),
            ),
            (
                "/data/file/extracted_from/generation",
                json!(GenerationId::from_bytes([8; 20])),
            ),
            (
                "/data/file/extracted_from/span/file",
                json!(value.data.file.id),
            ),
            ("/data/file/extracted_from/span/start_byte", json!(2)),
            ("/data/file/extracted_from/span/end_byte", json!(0)),
        ] {
            let mut forged = wire.clone();
            *forged.pointer_mut(pointer).expect("known wire field") = bad;
            assert!(
                serde_json::from_value::<SourceFindEnvelope>(forged).is_err(),
                "admitted {pointer}"
            );
        }
    }
}

#[test]
fn authenticated_resume_checks_request_before_returning_source_fingerprint() {
    let file = file();
    let budgets = budgets();
    let bound = binding(&file, &budgets);
    let position = SourceFindCursorPosition {
        next_byte: 20,
        matched_prefix: 3,
    };
    let key = [42; 32];
    let cursor = bound.issue(position, 1000, &key).unwrap();
    let resumed = bound.request().resume(&cursor, 1001, &key).unwrap();
    assert_eq!(resumed.position, position);
    assert_eq!(
        resumed.file_fingerprint,
        file.fingerprint(repository(), generation()).unwrap()
    );
    assert_eq!(bound.resume(&cursor, 1001, &key).unwrap(), position);
    assert!(bound.request().resume(&cursor, 301_001, &key).is_err());
    assert!(bound.request().resume(&cursor, 1001, &[43; 32]).is_err());
    for changed in 0..7 {
        let mut request = bound.request();
        match changed {
            0 => request.repository = RepositoryId::from_bytes([9; 16]),
            1 => request.generation = GenerationId::from_bytes([9; 20]),
            2 => request.file_id = FileId::from_bytes([9; 20]),
            3 => request.literal = "ABABA",
            4 => request.exposure_profile = ExposureProfile::Developer,
            5 => request.snapshot_id = [9; 32],
            6 => request.key_id += 1,
            _ => unreachable!(),
        }
        assert!(
            request.resume(&cursor, 1001, &key).is_err(),
            "scope change {changed}"
        );
    }
    let mut unserved_profile = bound.request();
    unserved_profile.response_profile = ResponseProfile::Evidence;
    assert!(
        unserved_profile.resume(&cursor, 1001, &key).is_err(),
        "raw cursor authentication also binds the independently validated compact representation"
    );
    let mut forged_context = bound.context().unwrap();
    forged_context.plan_fingerprint = [0; 32];
    // A valid envelope with changed source evidence can authenticate the request
    // but must still fail the post-read comparison against the actual file.
    let changed_file = AuthenticatedCursor::create(
        forged_context,
        [20_u64.to_le_bytes(), 3_u64.to_le_bytes()].concat(),
        1000,
        &key,
    )
    .unwrap();
    let changed_file = ContinuationCursor::parse(&changed_file.to_wire()).unwrap();
    let resumed = bound.request().resume(&changed_file, 1001, &key).unwrap();
    assert_ne!(
        resumed.file_fingerprint,
        file.fingerprint(repository(), generation()).unwrap()
    );
    assert!(bound.resume(&changed_file, 1001, &key).is_err());
}

#[test]
fn every_effective_budget_and_source_metadata_field_is_cursor_bound() {
    let file = file();
    let original_budgets = budgets();
    let bound = binding(&file, &original_budgets);
    let position = SourceFindCursorPosition {
        next_byte: 20,
        matched_prefix: 3,
    };
    let cursor = bound.issue(position, 1000, &[42; 32]).unwrap();
    for field in 0..14 {
        let mut changed = original_budgets.clone();
        match field {
            0 => changed.maximum_scan_bytes -= 1,
            1 => changed.maximum_matches -= 1,
            2 => changed.maximum_query_bytes -= 1,
            3 => changed.maximum_snapshot_bytes -= 1,
            4 => changed.maximum_matcher_memory_bytes -= 1,
            5 => changed.maximum_restore_bytes -= 1,
            6 => changed.maximum_response_memory_bytes -= 1,
            7 => changed.maximum_metadata_bytes -= 1,
            8 => changed.maximum_results -= 1,
            9 => changed.maximum_json_bytes -= 1,
            10 => changed.maximum_tokens -= 1,
            11 => changed.timeout_ms -= 1,
            12 => changed.maximum_verified_bytes -= 1,
            13 => changed.source_verification_duration_explicit = false,
            _ => unreachable!(),
        }
        assert!(
            binding(&file, &changed)
                .request()
                .resume(&cursor, 1001, &[42; 32])
                .is_err(),
            "budget {field}"
        );
    }
    for field in 0..7 {
        let mut changed = file.clone();
        match field {
            0 => changed.path = "other.e".to_owned(),
            1 => changed.language = "text".to_owned(),
            2 => changed.content_hash = ContentHash::from_bytes([8; 32]),
            3 => changed.byte_length = 102,
            4 => changed.encoding = SourceTextEncoding::Utf16Be,
            5 => changed.generated = true,
            6 => {
                changed.extracted_from = Some(SourceRef::new(
                    repository(),
                    generation(),
                    SourceSpan::new(FileId::from_bytes([8; 20]), 0, 120).unwrap(),
                    ContentHash::from_bytes([8; 32]),
                    None,
                ))
            }
            _ => unreachable!(),
        }
        assert!(
            binding(&changed, &original_budgets)
                .resume(&cursor, 1001, &[42; 32])
                .is_err(),
            "metadata {field}"
        );
    }
}

#[test]
fn cursor_rejects_zero_progress_eof_prefix_overflow_and_malformed_payloads() {
    let file = file();
    let budgets = budgets();
    let bound = binding(&file, &budgets);
    for position in [
        SourceFindCursorPosition {
            next_byte: 0,
            matched_prefix: 0,
        },
        SourceFindCursorPosition {
            next_byte: 100,
            matched_prefix: 0,
        },
        SourceFindCursorPosition {
            next_byte: 20,
            matched_prefix: 5,
        },
        SourceFindCursorPosition {
            next_byte: 2,
            matched_prefix: 3,
        },
    ] {
        assert!(bound.issue(position, 1000, &[42; 32]).is_err());
    }
    for payload in [vec![], vec![0; 15], vec![0; 17], vec![0; 16]] {
        let cursor =
            AuthenticatedCursor::create(bound.context().unwrap(), payload, 1000, &[42; 32])
                .unwrap();
        let wire = ContinuationCursor::parse(&cursor.to_wire()).unwrap();
        assert!(bound.request().resume(&wire, 1001, &[42; 32]).is_err());
    }
    let mut wide = file;
    wide.encoding = SourceTextEncoding::Utf16Le;
    for next_byte in [2, 3, 99] {
        assert!(
            binding(&wide, &budgets)
                .issue(
                    SourceFindCursorPosition {
                        next_byte,
                        matched_prefix: 0
                    },
                    1000,
                    &[42; 32]
                )
                .is_err()
        );
    }
}

#[test]
fn generated_source_find_schema_is_a_separate_twentieth_contract() {
    use crate::vertical::VerticalTool;
    assert_eq!(VerticalTool::ALL.len(), 20);
    assert_eq!(VerticalTool::ALL[19], VerticalTool::SourceFind);
    assert_eq!(
        VerticalTool::SourceFind.contract_version(),
        SOURCE_FIND_VERSION
    );
    let request = serde_json::to_value(input()).unwrap();
    let response = serde_json::to_value(SourceFindOutput::Success(envelope())).unwrap();
    for (direction, schema, fixture) in [
        (
            "input",
            VerticalTool::SourceFind.input_schema_json(),
            &request,
        ),
        (
            "output",
            VerticalTool::SourceFind.output_schema_json(),
            &response,
        ),
    ] {
        let schema: Value = serde_json::from_str(schema).unwrap();
        assert_eq!(
            schema["$id"],
            format!(
                "https://rootlight.local/schema/mcp/source.find/{direction}/{SOURCE_FIND_VERSION}"
            )
        );
        assert!(
            jsonschema::draft202012::new(&schema)
                .unwrap()
                .is_valid(fixture)
        );
    }
    let decoded: SourceFindInput = serde_json::from_value(request.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), request);
    let schema: Value = serde_json::from_str(VerticalTool::SourceFind.input_schema_json()).unwrap();
    let validator = jsonschema::draft202012::new(&schema).unwrap();
    for (profile, admitted) in [("compact", true), ("standard", false), ("evidence", false)] {
        let mut selected = request.clone();
        selected["response_profile"] = json!(profile);
        assert_eq!(validator.is_valid(&selected), admitted, "{profile}");
        assert_eq!(
            serde_json::from_value::<SourceFindInput>(selected).is_ok(),
            admitted,
            "{profile}"
        );
    }
    let decoded: SourceFindOutput = serde_json::from_value(response.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), response);
    assert_eq!(response["schema_version"], SOURCE_FIND_VERSION);
}

#[test]
fn admitted_long_paths_preserve_exact_metadata_and_cursor_identity() {
    let mut value = envelope();
    value.data.file.path = format!("{}name.e", "part/".repeat(13_106));
    assert_eq!(value.data.file.path.len(), 65_536);
    value.data.total_metadata_bytes =
        u64::try_from(value.data.file.path.len() + value.data.file.language.len()).unwrap();
    value.data.total_response_memory_bytes = value.data.total_metadata_bytes + 300;
    let fingerprint = value
        .data
        .file
        .fingerprint(repository(), generation())
        .unwrap();
    let decoded: SourceFindEnvelope =
        serde_json::from_value(serde_json::to_value(&value).unwrap()).unwrap();
    assert_eq!(decoded, value);
    assert_eq!(
        decoded
            .data
            .file
            .fingerprint(repository(), generation())
            .unwrap(),
        fingerprint
    );
    let schema = serde_json::to_value(schemars::schema_for!(SourceFindOutput)).unwrap();
    assert!(
        jsonschema::draft202012::new(&schema)
            .unwrap()
            .is_valid(&serde_json::to_value(SourceFindOutput::Success(value.clone())).unwrap())
    );
    let generated: Value =
        serde_json::from_str(crate::vertical::VerticalTool::SourceFind.output_schema_json())
            .unwrap();
    assert!(
        jsonschema::draft202012::new(&generated)
            .unwrap()
            .is_valid(&serde_json::to_value(SourceFindOutput::Success(value.clone())).unwrap())
    );
    value.data.file.path.push('x');
    assert!(
        value
            .data
            .file
            .fingerprint(repository(), generation())
            .is_err()
    );
    assert!(
        serde_json::from_value::<SourceFindEnvelope>(serde_json::to_value(value).unwrap()).is_err()
    );
    for path in [
        "/file.e",
        "a//file.e",
        "a/../file.e",
        "./file.e",
        "C:/file.e",
        "a\\file.e",
        "a\0file.e",
    ] {
        let mut value = envelope();
        value.data.file.path = path.to_owned();
        assert!(
            value
                .data
                .file
                .fingerprint(repository(), generation())
                .is_err(),
            "{path:?}"
        );
    }
}
