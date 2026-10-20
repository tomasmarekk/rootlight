//! Recovery admission rejects fabricated causes and file-set omissions.
//! Public byte and lossless-text paths share the same correlated explanation.

use super::*;
use crate::tests::{test_repository, test_source, wire_source_read};

fn fixture(files: &[FileId]) -> daemon::SourceReadRecovery {
    daemon::SourceReadRecovery {
        cause: Some(daemon::AggregateSourceRecoveryCause {
            source_inventory: Some(content_hash_to_wire(ContentHash::from_bytes([9; 32]))),
            members: 100,
            derived_members: 2,
            stage: daemon::AggregateSourceRecoveryStage::StructuralAppend as i32,
            resource: daemon::AggregateSourceRecoveryResource::Occurrences as i32,
            observed: 101,
            limit: 100,
            text_snapshot_bytes: 8192,
        }),
        files: files
            .iter()
            .enumerate()
            .map(|(index, file)| daemon::SourceRecoveryFile {
                file: Some(file_to_wire(*file)),
                text_search: match index % 3 {
                    0 => daemon::SourceRecoveryTextSearch::WholeSourceProjection as i32,
                    1 => daemon::SourceRecoveryTextSearch::MetadataOnlyCaptured as i32,
                    _ => daemon::SourceRecoveryTextSearch::UnsupportedEncoding as i32,
                },
            })
            .collect(),
    }
}

fn parse(
    wire: daemon::SourceReadRecovery,
    files: &[FileId],
) -> Result<Option<SourceReadRecovery>, ClientError> {
    parse_source_recovery(Some(wire), files.iter().copied(), CURRENT_PROTOCOL_MINOR)
}

#[test]
fn returned_file_set_is_exact_sorted_unique_and_independent_of_chunk_order() {
    let files = [1, 2, 3].map(|value| FileId::from_bytes([value; 20]));
    let wire = fixture(&files);
    let result = parse(wire.clone(), &[files[2], files[0], files[1], files[0]])
        .unwrap()
        .unwrap();
    assert_eq!(
        result.files.iter().map(|row| row.file).collect::<Vec<_>>(),
        files
    );
    assert_eq!(
        result.files[0].text_search,
        SourceTextSearchCoverage::WholeSourceProjection
    );
    assert_eq!(
        result.files[1].text_search,
        SourceTextSearchCoverage::MetadataOnlyCaptured
    );
    assert_eq!(
        result.files[2].text_search,
        SourceTextSearchCoverage::UnsupportedEncoding
    );
    for malformed in [
        {
            let mut w = wire.clone();
            w.files.swap(0, 1);
            w
        },
        {
            let mut w = wire.clone();
            w.files[1] = w.files[0].clone();
            w
        },
        {
            let mut w = wire.clone();
            w.files.pop();
            w
        },
        {
            let mut w = wire.clone();
            w.files[0].file = Some(file_to_wire(FileId::from_bytes([7; 20])));
            w
        },
        {
            let mut w = wire.clone();
            w.files[0].file = None;
            w
        },
        {
            let mut w = wire.clone();
            w.files[0].text_search = 0;
            w
        },
        {
            let mut w = wire.clone();
            w.files[0].text_search = 1234;
            w
        },
    ] {
        assert!(matches!(
            parse(malformed, &files),
            Err(ClientError::InvalidResponseCorrelation)
        ));
    }
    assert!(parse(fixture(&[]), &[]).is_err());
}

#[test]
fn cause_semantics_use_the_shared_ir_policy_and_reject_malformed_identity() {
    let file = FileId::from_bytes([1; 20]);
    let wire = fixture(&[file]);
    let valid = wire.cause.clone().unwrap();
    for cause in [
        {
            let mut c = valid.clone();
            c.members = 0;
            c
        },
        {
            let mut c = valid.clone();
            c.derived_members = c.members;
            c
        },
        {
            let mut c = valid.clone();
            c.observed = c.limit;
            c
        },
        {
            let mut c = valid.clone();
            c.observed = u64::MAX;
            c
        },
        {
            let mut c = valid.clone();
            c.text_snapshot_bytes = 0;
            c
        },
        {
            let mut c = valid.clone();
            c.stage = 0;
            c
        },
        {
            let mut c = valid.clone();
            c.resource = 0;
            c
        },
        {
            let mut c = valid.clone();
            c.resource = 999;
            c
        },
        {
            let mut c = valid.clone();
            c.stage = daemon::AggregateSourceRecoveryStage::GenerationText as i32;
            c
        },
        {
            let mut c = valid.clone();
            c.source_inventory = None;
            c
        },
    ] {
        let mut malformed = wire.clone();
        malformed.cause = Some(cause);
        assert!(matches!(
            parse(malformed, &[file]),
            Err(ClientError::InvalidResponseCorrelation)
        ));
    }
    let mut invalid_hash = wire.clone();
    invalid_hash
        .cause
        .as_mut()
        .unwrap()
        .source_inventory
        .as_mut()
        .unwrap()
        .value
        .pop();
    assert!(matches!(
        parse(invalid_hash, &[file]),
        Err(ClientError::InvalidIdentifier)
    ));
    let mut absent = wire;
    absent.cause = None;
    assert!(parse(absent, &[file]).is_err());
}

#[test]
fn generation_text_resources_round_trip_only_with_their_actual_stage() {
    let file = FileId::from_bytes([1; 20]);
    for resource in [
        daemon::AggregateSourceRecoveryResource::GenerationTextBytes,
        daemon::AggregateSourceRecoveryResource::GenerationEncodedTextBytes,
    ] {
        let mut wire = fixture(&[file]);
        let cause = wire.cause.as_mut().unwrap();
        cause.stage = daemon::AggregateSourceRecoveryStage::GenerationText as i32;
        cause.resource = resource as i32;
        assert!(parse(wire.clone(), &[file]).is_ok());
        wire.cause.as_mut().unwrap().stage =
            daemon::AggregateSourceRecoveryStage::StructuralAppend as i32;
        assert!(parse(wire, &[file]).is_err());
    }
}

#[test]
fn every_structural_resource_preserves_its_exact_typed_cause() {
    let file = FileId::from_bytes([1; 20]);
    for (wire_resource, expected) in [
        (
            daemon::AggregateSourceRecoveryResource::Files,
            AggregateCapacityResource::Files,
        ),
        (
            daemon::AggregateSourceRecoveryResource::Entities,
            AggregateCapacityResource::Entities,
        ),
        (
            daemon::AggregateSourceRecoveryResource::Occurrences,
            AggregateCapacityResource::Occurrences,
        ),
        (
            daemon::AggregateSourceRecoveryResource::Relations,
            AggregateCapacityResource::Relations,
        ),
        (
            daemon::AggregateSourceRecoveryResource::Provenance,
            AggregateCapacityResource::Provenance,
        ),
        (
            daemon::AggregateSourceRecoveryResource::SourceMappings,
            AggregateCapacityResource::SourceMappings,
        ),
        (
            daemon::AggregateSourceRecoveryResource::Coverage,
            AggregateCapacityResource::Coverage,
        ),
        (
            daemon::AggregateSourceRecoveryResource::SkippedRegions,
            AggregateCapacityResource::SkippedRegions,
        ),
        (
            daemon::AggregateSourceRecoveryResource::Diagnostics,
            AggregateCapacityResource::Diagnostics,
        ),
        (
            daemon::AggregateSourceRecoveryResource::Extensions,
            AggregateCapacityResource::Extensions,
        ),
        (
            daemon::AggregateSourceRecoveryResource::Records,
            AggregateCapacityResource::Records,
        ),
        (
            daemon::AggregateSourceRecoveryResource::NestedItems,
            AggregateCapacityResource::NestedItems,
        ),
        (
            daemon::AggregateSourceRecoveryResource::StringBytes,
            AggregateCapacityResource::StringBytes,
        ),
        (
            daemon::AggregateSourceRecoveryResource::ExtensionBytes,
            AggregateCapacityResource::ExtensionBytes,
        ),
        (
            daemon::AggregateSourceRecoveryResource::DiagnosticBytes,
            AggregateCapacityResource::DiagnosticBytes,
        ),
    ] {
        let mut wire = fixture(&[file]);
        wire.cause.as_mut().unwrap().resource = wire_resource as i32;
        assert_eq!(
            parse(wire, &[file]).unwrap().unwrap().cause.resource,
            expected
        );
    }
}

#[test]
fn older_protocols_never_accept_new_cause_and_absence_is_accepted_without_inference() {
    let file = FileId::from_bytes([1; 20]);
    assert!(parse_source_recovery(Some(fixture(&[file])), [file].into_iter(), 29).is_err());
    for minor in [1, 24, 29, 30] {
        assert_eq!(
            parse_source_recovery(None, [file].into_iter(), minor).unwrap(),
            None
        );
    }
}

#[test]
fn exact_bytes_and_lossless_text_keep_identical_cause_and_physical_references() {
    let source = test_source(4, 0, 3);
    let mut wire = wire_source_read(&source);
    wire.source_recovery = Some(fixture(&[source.file]));
    wire.context.as_mut().unwrap().tier = daemon::FirstSliceAnalysisTier::FirstSliceTierD as i32;
    wire.context.as_mut().unwrap().coverage_status =
        daemon::FirstSliceCoverageStatus::FirstSliceCoverageUnknown as i32;
    wire.chunks[0].tier = daemon::FirstSliceAnalysisTier::FirstSliceTierD as i32;
    let bytes = parse_source_read(
        wire.clone(),
        test_repository(),
        GenerationSelector::Active,
        std::slice::from_ref(&source),
        SourceReadOptions::new(),
    )
    .unwrap();
    assert_eq!(bytes.chunks[0].source, source);
    assert_eq!(bytes.chunks[0].content, b"abc");
    wire.total_text_bytes = 3;
    wire.chunks[0].encoding = daemon::SourceReadEncoding::LosslessText as i32;
    wire.chunks[0].physical_encoding = Some(daemon::SourcePhysicalEncoding::Utf8 as i32);
    wire.chunks[0].exact_content.clear();
    let text = crate::source_text::parse_source_text_read_for_minor(
        wire,
        test_repository(),
        GenerationSelector::Active,
        std::slice::from_ref(&source),
        SourceTextReadOptions::new(),
        CURRENT_PROTOCOL_MINOR,
    )
    .unwrap();
    assert_eq!(text.chunks[0].source, source);
    assert_eq!(text.chunks[0].content, "abc");
    assert_eq!(bytes.source_recovery, text.source_recovery);
}

#[test]
fn recovery_cannot_claim_complete_or_structural_analysis() {
    let source = test_source(4, 0, 3);
    let mut valid = wire_source_read(&source);
    valid.source_recovery = Some(fixture(&[source.file]));
    valid.context.as_mut().unwrap().tier = daemon::FirstSliceAnalysisTier::FirstSliceTierD as i32;
    valid.context.as_mut().unwrap().coverage_status =
        daemon::FirstSliceCoverageStatus::FirstSliceCoverageUnknown as i32;
    valid.chunks[0].tier = daemon::FirstSliceAnalysisTier::FirstSliceTierD as i32;
    for malformed in [
        {
            let mut w = valid.clone();
            w.context.as_mut().unwrap().coverage_status =
                daemon::FirstSliceCoverageStatus::FirstSliceCoverageComplete as i32;
            w
        },
        {
            let mut w = valid.clone();
            w.context.as_mut().unwrap().tier =
                daemon::FirstSliceAnalysisTier::FirstSliceTierC as i32;
            w
        },
        {
            let mut w = valid.clone();
            w.chunks[0].tier = daemon::FirstSliceAnalysisTier::FirstSliceTierB as i32;
            w
        },
    ] {
        assert!(matches!(
            parse_source_read(
                malformed,
                test_repository(),
                GenerationSelector::Active,
                std::slice::from_ref(&source),
                SourceReadOptions::new()
            ),
            Err(ClientError::InvalidResponseCorrelation)
        ));
    }
}
